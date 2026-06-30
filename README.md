# Farbstrom

Farbstrom is a secure, private, ultra-low-latency streaming and video conference platform, designed for realtime color accurate review sessions.

[![Documentation](https://img.shields.io/badge/docs-farbhaus.pt%2Ffarbstrom-2563eb)](https://docs.farbhaus.pt/farbstrom/)

> 📖 **Full user manual → <https://docs.farbhaus.pt/farbstrom/>**

## What it can do

- Stream sub-second ultra-low-latency video.
- Voice and video conference, including screen sharing.
- Session persistant chat and file sharing.
- Shared pointer for so that everyone agrees on what is being discussed.
- Host and participants roles. Host can moderate with admit, mute and kick priveleges.

Farbstrom Admins have access to a fully fledged suite of tools to manage, create and share rooms with hosts and participants 
in addition there is stream monitoring, file management, custom branding options as well login security management.

A unique concept of Fabstrom is the ability to assign different streamkeys to rooms on the fly allowing for flexible planning and organising of remote sessions

Finally there is *Farbplay* a macOS & iOS/iPadOS Player App for sub-second high quality HDR playback in development. 

**Admin Page**
<img width="1600" height="928" alt="rooms-tab" src="https://github.com/user-attachments/assets/75f93a37-3234-43ed-9e7a-edef7aba7e43" />
<img width="1000" height="1061" alt="room-modal" src="https://github.com/user-attachments/assets/aded6276-3dac-4967-bbe2-720581aa37bf" />
<img width="1600" height="928" alt="stream-keys" src="https://github.com/user-attachments/assets/11e9322b-6616-4b10-aed0-cb023e6b458f" />
<img width="1600" height="928" alt="files-tab" src="https://github.com/user-attachments/assets/83bb8a56-5211-4240-ba23-bfea723c6e0f" />
<img width="1600" height="928" alt="dashboard" src="https://github.com/user-attachments/assets/056b105a-27e6-4863-a967-e611b96b7654" />
<img width="1600" height="928" alt="branding" src="https://github.com/user-attachments/assets/03777288-18b9-4db5-bac8-d1b1ea994752" />
<img width="1600" height="928" alt="settings" src="https://github.com/user-attachments/assets/e76b29db-188a-45a2-92b8-1361379b30bd" />

**Room**
<img width="1600" height="928" alt="chat-panel" src="https://github.com/user-attachments/assets/9d1fb03f-e668-470b-bc79-60429075d9c3" />
<img width="1600" height="925" alt="pointer" src="https://github.com/user-attachments/assets/42825c5e-74ab-4ddc-9766-92521728a5ab" />
<img width="1600" height="928" alt="chat-panel" src="https://github.com/user-attachments/assets/55add4ce-2c42-496e-a202-f2774c4bda8d" />
<img width="1600" height="928" alt="screen-sharing" src="https://github.com/user-attachments/assets/b0c6fcd8-9eef-488e-b477-1ac672b81e5f" />

## Documentation

The full user manual is at **<https://docs.farbhaus.pt/farbstrom/>**. It covers a quickstart guide and includes complete documentation of the admin page and room features, as well as encoder setup for OBS and the Blackmagic Web Presenter.

Deployment information can be found in [DEPLOYMENT.md](docs/DEPLOYMENT.md).

## Quickstart

Here is a one line command to deploy Farbstrom on a fresh VPS or VM, all you have to do is replace the domain with your own:

```bash
curl -fsSL https://raw.githubusercontent.com/farbhaus/Farbstrom/main/install.sh | bash -s -- stream.yourdomain.com
```
Alternatively, ff you just want to try things out:

```bash
git clone https://github.com/farbhaus/Farbstrom && sudo ./deploy.sh localhost
```

This will run the whole stack locally if you just want a look. Flags, running behind an existing TLS proxy, manual `.env` setup, and local frontend development are all in [DEPLOYMENT.md](docs/DEPLOYMENT.md).

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
