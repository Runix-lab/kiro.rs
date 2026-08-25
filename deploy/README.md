# Deployment

`deploy.sh` — green-validate + swap deployment, meant to run **on the host** that
serves the live instance.

```bash
# on the server
bash deploy/deploy.sh master          # build origin/master, smoke-test, swap
bash deploy/deploy.sh <sha>           # pin an exact commit
bash deploy/deploy.sh master --skip-green   # emergencies only
```

What it does:

1. Fetches the repo into `~/kiro-src` and builds `kiro-rs:<shortsha>` with Docker.
2. **Green smoke**: runs the new image against an *isolated* data dir
   (config only — no `credentials.json`, see ARCHITECTURE.md §8 on the
   single-writer constraint) on `127.0.0.1:18080`, asserts `/admin` serves and
   `/v1/models` gates with 401.
3. **Swap**: rewrites the `image:` line in `~/kiro-rs/docker-compose.yml`
   (backing it up first) and `docker compose up -d`. Expected service gap is
   ~10-15s: in-app graceful drain is capped at 8s, then startup loads
   credentials before serving.
4. **Health check**: polls `/admin` on the live port for 45s; on failure it
   restores the previous image line and brings the old version back up.

Why not two live instances behind nginx: the app is the single writer of its
data directory (refresh-token rotation writes back to `credentials.json`), so
overlapping instances corrupt credentials. The green instance therefore only
validates the build, and the swap itself is a short stop-start.

## Pricing config

Cost/discount features read `pricing` from `config.json` (see
`config.example.json`): `creditUsdRate` (default 0.02 $/credit) plus optional
per-model official prices in $/Mtok. Claude-family models have built-in
defaults; models without a price entry show "—" in the UI instead of $0.
Changes require a restart.
