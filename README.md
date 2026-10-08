# X poster (login session backend)

Backend kecil yang posting teks + gambar/video ke X lewat session login yang tersimpan (Playwright), dan diekspos sebagai endpoint HTTP untuk n8n. Tidak memakai API resmi X.

```
n8n (HTTP Request) -> x-poster (FastAPI) -> Chromium + saved session -> x.com
```

## 1. Simpan session login (sekali saja, di komputer yang punya layar)

```bash
pip install -r requirements.txt
playwright install chromium
python login.py
```

Login manual di jendela yang terbuka (termasuk 2FA), tunggu timeline muncul, lalu tekan Enter di terminal.
Hasilnya: `data/storage_state.json`. **File ini sama sensitifnya dengan password.** Jangan di-commit ke git.

## 2. Jalankan

Lokal:
```bash
export API_TOKEN="$(openssl rand -hex 32)"
uvicorn server:app --host 127.0.0.1 --port 8000
```

Docker (deploy):
```bash
echo "API_TOKEN=$(openssl rand -hex 32)" > .env
mkdir -p data && cp /path/ke/storage_state.json data/
docker compose up -d --build
curl http://127.0.0.1:8000/health   # {"ok":true,"session_saved":true}
```

## 3. Endpoint

Semua POST butuh header `x-token: <API_TOKEN>`.

**`POST /post`** (JSON, media dari URL publik)
```json
{ "text": "Caption kamu", "media_urls": ["https://.../gambar.jpg"] }
```

**`POST /post/upload`** (multipart, kirim file langsung dari n8n)
- field `text`: caption
- field `files`: binary (ulang field untuk banyak gambar)

Aturan media: maksimal 4 gambar (jpg/png/webp), atau 1 video (mp4/mov), atau 1 gif. Tidak boleh dicampur.

Respons:
- `{"ok": true, "status": "posted", "url": ...}` berhasil
- `{"ok": null, "status": "unconfirmed"}` post mungkin sudah terkirim tapi tidak terkonfirmasi, **cek manual dulu sebelum mengulang**
- `401` session habis, jalankan `login.py` lagi
- `429` terlalu cepat (default minimal 60 detik antar post)
- `409` ada post lain yang sedang berjalan

## 4. Setting node HTTP Request di n8n

| Field | Isi |
|---|---|
| Method | POST |
| URL | `http://x-poster:8000/post` (satu compose network) atau `http://host.docker.internal:8000/post` |
| Headers | `x-token` = API_TOKEN |
| Body (JSON) | `{{ JSON.stringify({ text: $json.caption, media_urls: [$json.media_url] }) }}` |
| Options > Timeout | 600000 (video bisa lama diupload) |
| Settings > Retry On Fail | **Matikan**, supaya tidak terjadi post ganda |

Untuk kirim file binary: Body Content Type = `Form-Data`, tambah parameter `text` (Text) dan `files` (n8n Binary File, isi nama field binary-nya), URL `/post/upload`.

Contoh workflow siap import ada di `n8n_workflow.json` (Import from File).

## 5. Catatan penting

- **Melanggar ToS X.** Akun bisa kena limit, shadowban, atau suspend. Pakai akun cadangan dulu, jaga frekuensi rendah dan variatif.
- **Selector bisa berubah.** Semua selector ada di bagian atas `server.py`. Kalau gagal, lihat screenshot di `data/debug/`.
- Session bisa kedaluwarsa atau diminta verifikasi ulang oleh X. Kalau dapat `401`, jalankan `login.py` lagi.
- Jangan buka port ke internet. Biarkan hanya `127.0.0.1` atau jaringan Docker internal.
- URL media diblokir kalau menunjuk ke IP privat (anti-SSRF). Set `ALLOW_PRIVATE_MEDIA_URLS=true` hanya kalau media kamu ada di host internal.
- Batas teks default 280 karakter. Ubah lewat `MAX_TEXT_CHARS` kalau akunmu Premium.
- Video: batas durasi dan ukuran mengikuti aturan akun X-mu.
