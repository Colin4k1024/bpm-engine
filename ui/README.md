# Execution Trace UI

Single-instance, read-only Execution Trace UI for the BPM engine: Instance header, process diagram (nodes + token highlight), execution timeline, and event detail panel.

## Run

1. Start the REST API (from repo root):
   ```bash
   cargo run -p bpm-server-rest
   ```
   Server listens on `http://localhost:3000`.

2. Start the UI (from this directory):
   ```bash
   npm install
   npm run dev
   ```
   UI runs at `http://localhost:5173` and proxies `/api` to the backend.

3. Open `http://localhost:5173`, enter a process instance ID (e.g. from `POST /api/v1/process-instances`), then **View Trace**.

## Build

```bash
npm run build
```
Output is in `dist/`. Serve it from the same host as the API or configure CORS and API base URL.
