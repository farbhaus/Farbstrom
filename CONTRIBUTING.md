# Contributing to Farbstrom

Thanks for your interest in improving Farbstrom.

## License of contributions

Farbstrom is released under the **GNU Affero General Public License v3.0**
(see [LICENSE](LICENSE)). By contributing, you agree that your contributions
are licensed under the same AGPL-3.0 terms.

## Developer Certificate of Origin (DCO)

This project uses the [Developer Certificate of Origin](https://developercertificate.org/)
rather than a CLA. The DCO is a lightweight statement that you have the right to
submit the code you are contributing. You certify the DCO by adding a
`Signed-off-by` line to every commit:

```
Signed-off-by: Your Name <your.email@example.com>
```

Git adds this automatically when you commit with `-s`:

```bash
git commit -s -m "Your message"
```

The full text you are certifying:

> **Developer Certificate of Origin 1.1**
>
> By making a contribution to this project, I certify that:
>
> (a) The contribution was created in whole or in part by me and I have the
>     right to submit it under the open source license indicated in the file; or
>
> (b) The contribution is based upon previous work that, to the best of my
>     knowledge, is covered under an appropriate open source license and I have
>     the right under that license to submit that work with modifications,
>     whether created in whole or in part by me, under the same open source
>     license (unless I am permitted to submit under a different license), as
>     indicated in the file; or
>
> (c) The contribution was provided directly to me by some other person who
>     certified (a), (b) or (c) and I have not modified it.
>
> (d) I understand and agree that this project and the contribution are public
>     and that a record of the contribution (including all personal information
>     I submit with it, including my sign-off) is maintained indefinitely and
>     may be redistributed consistent with this project or the open source
>     license(s) involved.

## Development setup

See the root [README.md](README.md) for the project. Before opening a pull
request, please make sure the CI gates pass locally:

```bash
# Backend
cd backend
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test

# Frontend
cd ../frontend
npm run typecheck
```

## Third-party notices

Farbstrom bundles open-source dependencies whose attribution notices are
collected in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md), regenerated
with [`cargo-about`](https://github.com/EmbarkStudios/cargo-about):

```bash
cd backend
cargo about generate about.hbs -o ../THIRD_PARTY_NOTICES.md
```

Run this whenever dependencies change.
