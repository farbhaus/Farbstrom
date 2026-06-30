# Farbstrom

Private low-latency streaming platform for color-grading review sessions.

[![Documentation](https://img.shields.io/badge/docs-farbhaus.pt%2Ffarbstrom-2563eb)](https://docs.farbhaus.pt/farbstrom/)
[![License: AGPL-3.0](https://img.shields.io/badge/license-AGPL--3.0-blue)](LICENSE)

> 📖 **Full user manual → <https://docs.farbhaus.pt/farbstrom/>**

## What is Farbstrom

Farbstrom streams a colorist's grade to remote reviewers in near real time, with little quality loss, while everyone talks over voice and video. You run it on your own server, so the footage stays on infrastructure you control.

The colorist or studio runs the session as presenter. Reviewers (directors, DPs, clients) join from a browser to watch and give feedback. There's nothing to install.

## What it can do

- Low-latency video in the browser, including HDR.
- Voice and video conference, screen sharing, and a watch-only mode for people who just want to look.
- Presenter and viewer roles. Only an admin can make someone a presenter.
- Chat, file sharing, and a shared pointer for pointing at an exact spot on screen.
- Kick and mute for moderation, plus optional room passwords and waiting rooms.
- Scheduled rooms with an expiry time, and a per-room choice of delivery mode.
- Custom branding (logo and colors) per deployment.
- An optional native SRT viewer (Farbplay) that joins from a room link for full-quality HDR playback.

<img width="1920" height="1100" alt="1" src="https://github.com/user-attachments/assets/820d33de-1370-49be-9d23-c5a955fd644c" />
<img width="1920" height="1100" alt="2" src="https://github.com/user-attachments/assets/423c95db-77f3-4e69-b01f-896a959d1917" />
<img width="1920" height="1100" alt="3" src="https://github.com/user-attachments/assets/01d2391b-380e-4651-a42b-0bc9fed061a4" />
<img width="1920" height="1100" alt="4" src="https://github.com/user-attachments/assets/19a7933d-ab91-4530-962f-2b35db0c206c" />
<img width="1920" height="1100" alt="5" src="https://github.com/user-attachments/assets/55be556b-3c87-4279-bfee-8a42bd1bf5eb" />
<img width="1920" height="1100" alt="6" src="https://github.com/user-attachments/assets/a6e9c95c-7f28-4f8c-b4e1-de7fdd32dcb3" />
<img width="1920" height="1100" alt="7" src="https://github.com/user-attachments/assets/1d77247b-d5e4-4498-80e1-338ee9044aec" />
<img width="1920" height="1100" alt="8" src="https://github.com/user-attachments/assets/ac715448-671a-4394-99c0-05390fe0637c" />
<img width="1920" height="1100" alt="9" src="https://github.com/user-attachments/assets/7418bb42-905c-4875-b5d5-937bcf0eee48" />
<img width="1920" height="1100" alt="10" src="https://github.com/user-attachments/assets/c74b0a23-2cbd-4a9d-9e4b-09bef8e98f15" />
<img width="1920" height="1100" alt="11" src="https://github.com/user-attachments/assets/596351a9-ff52-4d71-bbff-c883014dd18c" />

## Documentation

The full user manual is at **<https://docs.farbhaus.pt/farbstrom/>**. It covers getting started, the admin guide (rooms, stream keys, files, dashboard, branding, settings), encoder setup for OBS and the Blackmagic Web Presenter, and using a room (conference, devices, chat, files, layout, pointer, HDR review, keyboard shortcuts).

Self-hosting and configuration is in [DEPLOYMENT.md](docs/DEPLOYMENT.md).

## Quickstart

On a fresh VPS that only runs Farbstrom, one command installs the prerequisites, generates secrets, gets a Let's Encrypt cert, and prints the admin password once:

```bash
sudo ./deploy.sh stream.yourdomain.com
```

You don't even need to clone the repo first:

```bash
curl -fsSL https://raw.githubusercontent.com/farbhaus/Farbstrom/main/install.sh | bash -s -- stream.yourdomain.com
```

`sudo ./deploy.sh localhost` runs the whole stack locally if you just want a look. Flags, running behind an existing TLS proxy, manual `.env` setup, and local frontend development are all in [DEPLOYMENT.md](docs/DEPLOYMENT.md).

## Built with

Everything runs in a single container: OvenMediaEngine for broadcast ingest and delivery, LiveKit for participant voice/video, a Rust/Axum backend, and Caddy for TLS and routing. The architecture diagram and full breakdown are in [DEPLOYMENT.md](docs/DEPLOYMENT.md#architecture).

## License

Farbstrom is licensed under the GNU Affero General Public License v3.0 (AGPL-3.0), see [LICENSE](LICENSE). You're free to use, study, modify, and self-host it, but if you run a modified version as a network service you have to make your modified source available to its users.

Contributions are accepted under the same license via the Developer Certificate of Origin, see [CONTRIBUTING.md](docs/CONTRIBUTING.md). Dependency attributions are in [THIRD_PARTY_NOTICES.md](docs/THIRD_PARTY_NOTICES.md).

## Acknowledgements

Farbstrom is built on these open-source projects:

- [OvenMediaEngine](https://github.com/AirenSoft/OvenMediaEngine) — broadcast ingest/delivery engine (AGPL-3.0)
- [OvenPlayer](https://github.com/AirenSoft/OvenPlayer) — LLHLS/WebRTC player (MIT)
- [LiveKit](https://github.com/livekit/livekit) — WebRTC SFU for participant conference (Apache-2.0)
- [Caddy](https://github.com/caddyserver/caddy) — TLS termination and routing (Apache-2.0)
- [Axum](https://github.com/tokio-rs/axum) and the broader Rust/Tokio ecosystem (MIT)
- [hls.js](https://github.com/video-dev/hls.js) — HLS playback fallback (Apache-2.0)

…and the many crates listed in [THIRD_PARTY_NOTICES.md](docs/THIRD_PARTY_NOTICES.md).
