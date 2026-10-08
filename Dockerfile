# Playwright image version must match the playwright version in requirements.txt
FROM mcr.microsoft.com/playwright/python:v1.49.0-jammy

WORKDIR /app
COPY requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt
COPY server.py login.py ./

ENV STATE_FILE=/data/storage_state.json \
    DEBUG_DIR=/data/debug

VOLUME /data
EXPOSE 8000
CMD ["uvicorn", "server:app", "--host", "0.0.0.0", "--port", "8000"]
