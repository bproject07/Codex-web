# Screenshot provenance

The images in this directory show the real browser interface connected to the
repository's deterministic demo PTY. The terminal text is synthetic and does
not come from Codex CLI, an AI model, a user account, or a private project.

## Files

- `desktop-terminal.png` — desktop terminal at 1440 × 900 CSS pixels
- `session-manager.png` — desktop session manager at 1440 × 900 CSS pixels
- `mobile-terminal.png` — mobile terminal at 390 × 844 CSS pixels

## Reproducing the demo

Build the frontend and backend, then start a disposable loopback server with a
neutral project directory and the platform-specific fixture:

Windows:

```powershell
New-Item -ItemType Directory -Force -Path "C:\demo-project" | Out-Null
.\server\target\release\codex-web.exe `
  --project "C:\demo-project" `
  --command ".\scripts\fixtures\demo-terminal.cmd" `
  --host 127.0.0.1 `
  --port 18800 `
  --token "replace-with-a-disposable-random-token" `
  --no-open-browser
```

Linux:

```bash
mkdir -p /tmp/demo-project
./server/target/release/codex-web \
  --project "/tmp/demo-project" \
  --command "./scripts/fixtures/demo-terminal.sh" \
  --host 127.0.0.1 \
  --port 18800 \
  --token "replace-with-a-disposable-random-token" \
  --no-open-browser
```

Open the authenticated URL, wait until the token has been removed from the
address bar, and capture only the page viewport without browser chrome.

## Privacy checklist

Before committing a screenshot, inspect the pixels and confirm that it contains
none of the following:

- account, person, organization, company, device, or host names;
- email addresses, credentials, tokens, or authenticated URLs;
- real IP addresses, DNS names, or private service details;
- terminal history, personal filesystem paths, or private project names;
- live Codex conversations or model output.

Use only neutral session names and the deterministic fixture. Do not edit a
private screenshot by merely covering sensitive pixels; create a clean capture
from the disposable demo environment instead.
