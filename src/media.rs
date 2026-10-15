use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::StreamExt;
use rand::Rng;
use reqwest::redirect::Policy;
use tokio::fs::File;
use tokio::io::AsyncWriteExt;
use url::Url;

use crate::config::Config;
use crate::error::AppError;

const IMAGE_EXT: &[&str] = &[".jpg", ".jpeg", ".png", ".webp"];
const ALLOWED_EXT: &[&str] = &[".jpg", ".jpeg", ".png", ".webp", ".gif", ".mp4", ".mov"];

pub fn ext_from_content_type(content_type: &str) -> &'static str {
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    match mime.as_str() {
        "image/jpeg" => ".jpg",
        "image/png" => ".png",
        "image/webp" => ".webp",
        "image/gif" => ".gif",
        "video/mp4" => ".mp4",
        "video/quicktime" => ".mov",
        _ => "",
    }
}

pub fn ext_for(path_or_url_path: &str, content_type: &str) -> String {
    if let Some(ext) = Path::new(path_or_url_path)
        .extension()
        .and_then(|e| e.to_str())
    {
        let dot_ext = format!(".{}", ext.to_ascii_lowercase());
        if ALLOWED_EXT.contains(&dot_ext.as_str()) {
            return dot_ext;
        }
    }
    ext_from_content_type(content_type).to_string()
}

pub fn is_allowed_ext(ext: &str) -> bool {
    ALLOWED_EXT.contains(&ext)
}

pub fn random_filename(ext: &str) -> String {
    let mut bytes = [0u8; 6];
    rand::rng().fill(&mut bytes);
    let mut hex = String::with_capacity(12 + ext.len());
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(&mut hex, "{b:02x}");
    }
    hex.push_str(ext);
    hex
}

pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_public_v4(mapped);
            }
            !v6.is_loopback()
                && !v6.is_unspecified()
                && !v6.is_multicast()
                && (v6.segments()[0] & 0xfe00) != 0xfc00
                && (v6.segments()[0] & 0xffc0) != 0xfe80
                && !(v6.segments()[0] == 0x2001 && v6.segments()[1] == 0x0db8)
        }
    }
}

fn is_public_v4(v4: Ipv4Addr) -> bool {
    let o = v4.octets();
    !v4.is_private()
        && !v4.is_loopback()
        && !v4.is_link_local()
        && !v4.is_broadcast()
        && !v4.is_documentation()
        && !v4.is_unspecified()
        && !v4.is_multicast()
        && o[0] != 0
        && !(o[0] == 100 && (o[1] & 0b1100_0000) == 64)
        && !(o[0] == 192 && o[1] == 0 && o[2] == 0)
        && !(o[0] == 198 && (o[1] == 18 || o[1] == 19))
        && o[0] < 240
}

async fn resolve_and_check_url(raw_url: &str, allow_private: bool) -> Result<(Url, Vec<SocketAddr>), AppError> {
    let parsed = Url::parse(raw_url)
        .map_err(|_| AppError::bad_request(format!("invalid media url: {raw_url}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(AppError::bad_request(format!("invalid media url: {raw_url}")));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| AppError::bad_request(format!("invalid media url: {raw_url}")))?;
    let port = parsed.port_or_known_default().unwrap_or(80);

    if allow_private {
        return Ok((parsed, Vec::new()));
    }

    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| AppError::bad_request(format!("cannot resolve media host: {host}")))?
        .collect();

    if addrs.is_empty() {
        return Err(AppError::bad_request(format!("cannot resolve media host: {host}")));
    }

    for addr in &addrs {
        if !is_public_ip(addr.ip()) {
            return Err(AppError::bad_request("media url must point to a public host"));
        }
    }

    Ok((parsed, addrs))
}

pub async fn download_media(url: &str, dest_dir: &Path, config: &Config) -> Result<PathBuf, AppError> {
    let mut current_url = url.to_string();
    let max_mb = config.max_media_bytes / (1024 * 1024);

    // Follow up to 4 redirects manually so every hop is re-checked against SSRF rules.
    for _ in 0..4 {
        let (parsed, addrs) =
            resolve_and_check_url(&current_url, config.allow_private_media_urls).await?;

        let mut builder = reqwest::Client::builder()
            .redirect(Policy::none())
            .timeout(Duration::from_secs(60));

        if !addrs.is_empty() {
            if let Some(host) = parsed.host_str() {
                if host.parse::<IpAddr>().is_err() {
                    builder = builder.resolve_to_addrs(host, &addrs);
                }
            }
        }

        let client = builder
            .build()
            .map_err(|e| AppError::bad_gateway(format!("failed to download media: {e}")))?;

        let resp = client
            .get(parsed.as_str())
            .send()
            .await
            .map_err(|e| AppError::bad_gateway(format!("failed to download media: {e}")))?;

        let status = resp.status();
        if matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308) {
            let loc = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| AppError::bad_gateway("redirect missing location header"))?;
            let next = parsed
                .join(loc)
                .map_err(|_| AppError::bad_request(format!("invalid redirect url: {loc}")))?;
            current_url = next.to_string();
            continue;
        }

        let resp = resp
            .error_for_status()
            .map_err(|e| AppError::bad_gateway(format!("failed to download media: {e}")))?;

        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let ext = ext_for(parsed.path(), content_type);
        if !is_allowed_ext(&ext) {
            return Err(AppError::unsupported_media(format!(
                "unsupported media type for {current_url}"
            )));
        }

        let dest = dest_dir.join(random_filename(&ext));
        let mut file = File::create(&dest)
            .await
            .map_err(|e| AppError::bad_gateway(format!("failed to create temp file: {e}")))?;

        let mut size: u64 = 0;
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk =
                chunk.map_err(|e| AppError::bad_gateway(format!("failed to download media: {e}")))?;
            size = size.saturating_add(chunk.len() as u64);
            if size > config.max_media_bytes {
                return Err(AppError::payload_too_large(format!(
                    "media larger than {max_mb} MB"
                )));
            }
            file.write_all(&chunk)
                .await
                .map_err(|e| AppError::bad_gateway(format!("failed to write temp file: {e}")))?;
        }
        file.flush()
            .await
            .map_err(|e| AppError::bad_gateway(format!("failed to flush temp file: {e}")))?;

        return Ok(dest);
    }

    Err(AppError::bad_request("too many redirects"))
}

pub fn validate_post(text: &str, media: &[PathBuf], max_text_chars: usize) -> Result<(), AppError> {
    if text.trim().is_empty() && media.is_empty() {
        return Err(AppError::bad_request("need text and/or media"));
    }
    if text.chars().count() > max_text_chars {
        return Err(AppError::bad_request(format!(
            "text longer than {max_text_chars} chars"
        )));
    }
    if media.is_empty() {
        return Ok(());
    }

    let mut image_count = 0usize;
    let mut non_image_count = 0usize;
    for path in media {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{}", e.to_ascii_lowercase()))
            .unwrap_or_default();
        if IMAGE_EXT.contains(&ext.as_str()) {
            image_count += 1;
        } else {
            non_image_count += 1;
        }
    }

    if non_image_count > 0 && media.len() > 1 {
        return Err(AppError::bad_request("a video or gif must be posted alone"));
    }
    if image_count > 4 {
        return Err(AppError::bad_request("max 4 images per post"));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_ip_rejects_private_and_loopback() {
        assert!(!is_public_ip("127.0.0.1".parse().unwrap()));
        assert!(!is_public_ip("10.0.0.1".parse().unwrap()));
        assert!(!is_public_ip("192.168.1.1".parse().unwrap()));
        assert!(!is_public_ip("172.16.0.1".parse().unwrap()));
        assert!(!is_public_ip("169.254.169.254".parse().unwrap()));
        assert!(!is_public_ip("100.64.0.1".parse().unwrap()));
        assert!(!is_public_ip("::1".parse().unwrap()));
        assert!(!is_public_ip("::ffff:127.0.0.1".parse().unwrap()));
        assert!(is_public_ip("93.184.216.34".parse().unwrap()));
    }

    #[test]
    fn validate_post_enforces_x_media_rules() {
        assert!(validate_post("   ", &[], 280).is_err());
        assert!(validate_post("hello", &[], 280).is_ok());
        assert!(validate_post(&"a".repeat(281), &[], 280).is_err());

        let imgs: Vec<PathBuf> = (0..5).map(|i| PathBuf::from(format!("{i}.jpg"))).collect();
        assert!(validate_post("", &imgs[..4], 280).is_ok());
        assert!(validate_post("", &imgs, 280).is_err());

        let mixed = vec![PathBuf::from("a.mp4"), PathBuf::from("b.jpg")];
        assert!(validate_post("", &mixed, 280).is_err());
    }

    #[test]
    fn ext_detection_falls_back_to_content_type() {
        assert_eq!(ext_for("/media/photo.PNG", "application/octet-stream"), ".png");
        assert_eq!(ext_for("/download", "video/mp4; charset=binary"), ".mp4");
        assert_eq!(ext_for("/download.exe", "application/x-msdownload"), "");
    }
}
