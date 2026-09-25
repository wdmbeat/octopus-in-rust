# fafcn ↔ FAF Integration — Design Document

Scope: this document owns **everything about integrating fafcn with FAF** — the
OAuth2 identity integration, the communication thread with the FAF team, the
credential checklist, and open questions raised by FAF (e.g. gamedata ownership
checks, section 2.2). The community gallery (sections 4.3, 5.2–5.3) is the **first
consumer** of the FAF identity; its design stays here for context, but gallery work
is **blocked** until the OAuth login round-trip works (section 2.1).

Status: **OAuth approved, awaiting credentials** — FAF admin (Brutus5000) agreed on
2026-08 to register the client (consent-screen name: `fafcn`); `client_id` /
`client_secret` not yet received. Next: section 2.1 checklist.
Author: fafcn team
Date: 2026-08-19

---

## Following up

- [push action from FAF community](https://faforever.zulipchat.com/#narrow/channel/203478-general/topic/Chinese.20community.2FChina/near/617382147)

## 1. Vision

Integrate fafcn with FAF so the website can grow from a tool site into a small
**community platform that fuses players' passion** — with identity delegated to FAF
as the foundation:

0. **FAF identity (this doc's core)** — players log in with their existing FAF
   account via FAF's OAuth2. We store no accounts, no passwords, no user profiles —
   only sessions. Admins are simply a hardcoded list of FAF usernames. Everything
   below builds on this.

1. **Living hero** — the frontpage hero stops being one hardcoded screenshot. It
   crossfades between several images while slowly zooming in (Ken Burns effect, the
   same feel as ee2x.cn), making the page feel alive.
2. **Community gallery** — players upload their own game screenshots with an optional
   caption. Uploads are invisible until an admin approves them; admins can mark the
   best ones as **featured**, and featured images become the frontpage hero slideshow.

Later increments (out of scope here): likes/comments, per-user gallery pages, FAF
avatar/rating on cards, stats/replay widgets (may need extra OAuth scopes).

---

## 2. Message to the FAF developer (sent 2026-08)

> **Outcome:** Brutus5000 agreed to register the client and asked for the service name
> shown on the consent screen → we answered **`fafcn`**. Credentials pending.
> Sheikah separately raised a question about how the gamedata mirror interacts with
> FAF's ownership checks — tracked in section 2.2.

Original message:

> @**Brutus5000** Good afternoon!
>
> I'm from the Chinese FAF community (FAFCN). Until recently we were just a private QQ
> group — not great for helping Chinese RTS players discover and start playing FA.
>
> That's changing: we're building a public website — https://faforever.cn:60/ — with two
> goals:
>
> 1. Introduce FA to Chinese RTS players (China has a huge RTS player base that has
>    never heard of FA).
> 2. Smooth out the onboarding process for playing on FAF (gamedata mirror, unit
>    database, build-order simulator, Chinese-language guides).
>
> As you can see, the site currently has no login at all. We'd like to add community
> features (screenshot gallery, player showcases, possibly stats/replay widgets like
> the FAF Discord bot has) — and for that we'd like players to log in **with their FAF
> accounts** via OAuth2.
>
> Concretely, we'd like to integrate exactly like faf-qai's Discord account linking
> does:
>
> - **Standard authorization code flow**, server-side (confidential client). Users
>   authenticate on FAF's own authorization page; we only receive the authorization
>   code and exchange it for a token.
> - **Scope: `public_profile` only** — we just need the player's FAF id and username.
> - We store **no FAF credentials and no user profiles** on our side — only an
>   ephemeral session cookie. All authentication stays fully under FAF's control; we
>   only wrap a thin layer of community metadata (e.g. "this gallery image was uploaded
>   by player X") around the FAF identity.
>
> Could you register an OAuth2 client for us? Details:
>
> - Redirect URIs:
>   - prod: `https://faforever.cn:60/api/auth/callback` (note the non-standard port)
>   - dev: `http://localhost:3000/api/auth/callback`
> - Grant type: authorization code
> - Scope: `public_profile`
>
> If a test-environment client (test.faforever.com) is available, we'd gladly take one
> as well.
>
> Any concerns or suggestions are very welcome. Thanks a lot!

### What we need back from FAF

1. **`client_id` + `client_secret`** for a confidential client with the authorization
   code grant.
2. **Both redirect URIs whitelisted exactly** — including the `:60` port on the
   prod one (Hydra requires exact matches; if the port is a problem, we move the site
   to standard 443 first).
3. **Endpoint confirmation** (verified by probing, 2026-08-24): the authorization
   server is **Ory Hydra at `https://hydra.faforever.com`** —
   `GET /oauth2/auth` (unknown client → 302 to `invalid_client` error page) and
   `POST /oauth2/token` (bad credentials → 401 JSON `invalid_client`).
   The old assumption `https://api.faforever.com/oauth2/*` is **outdated**: those
   paths now answer `401` with `WWW-Authenticate: Bearer resource_metadata=…`.
4. **Which endpoint returns the user's identity** with the token — `GET
   https://api.faforever.com/me` exists (401 without a Bearer token), shape to be
   confirmed with a real token; specifically that `public_profile` includes the
   **user id and login name**.
5. Optional: a **test-environment client** (test.faforever.com), and whether tokens
   can read public API data (player stats, replays) for future Discord-bot-like
   features (may need extra scopes later).

Reference implementation proving third-party viability:
[FAForever/faf-qai](https://github.com/FAForever/faf-qai) — "FAF OAuth2 Application
Setup: contact FAF administrators to create an OAuth2 application … receive your FAF
Client ID and FAF Client Secret … scope `public_profile`".

**Graceful degradation**: if the OAuth env vars are unset, the login endpoint returns
`503 unavailable` and everything else (gallery viewing, static hero fallback) works
normally — same pattern as our existing gamedata upload token.

### 2.1 Action checklist — once FAF sends the credentials

Do these **in order**; each step gates the next.

1. **Verify registration before writing any code** (client_id is not sensitive; the
   secret is never needed for these probes). A registered client redirects to FAF's
   login page; an unknown one redirects to an `invalid_client` error page:

   ```bash
   # dev redirect URI
   curl -sS -o /dev/null -w '%{http_code} %{redirect_url}\n' \
     'https://hydra.faforever.com/oauth2/auth?response_type=code&client_id=<CLIENT_ID>&redirect_uri=http%3A%2F%2Flocalhost%3A3000%2Fapi%2Fauth%2Fcallback&scope=public_profile&state=test'

   # prod redirect URI (confirms the :60 port was whitelisted)
   curl -sS -o /dev/null -w '%{http_code} %{redirect_url}\n' \
     'https://hydra.faforever.com/oauth2/auth?response_type=code&client_id=<CLIENT_ID>&redirect_uri=https%3A%2F%2Ffaforever.cn%3A60%2Fapi%2Fauth%2Fcallback&scope=public_profile&state=test'
   ```

   - Redirect to a login/consent page → registered **and** that redirect URI is
     whitelisted.
   - Error page with `invalid_client` → not registered (or wrong client_id).
   - Error mentioning `redirect_uri` → client exists but that URI is not whitelisted
     (expected for prod if Hydra rejected the non-standard port — then we move the
     site to 443 first, as noted above).

2. **Store the credentials** in `apps/fafcn-server/.env` (gitignored, never commit):
   `FAFCN_OAUTH_CLIENT_ID=…` / `FAFCN_OAUTH_CLIENT_SECRET=…`.
3. **Implement** backend auth + gallery per section 4. OAuth endpoints default to
   `https://hydra.faforever.com` and stay env-overridable (for the test environment).
4. **Local smoke test** (rollout phase 3): browser login round-trip via
   `http://localhost:3000/api/auth/login`; then `GET /api/auth/me` must return the FAF
   user id + username — this confirms the real `/me` response shape; update section
   4.2 if it differs from the assumption.
5. **Deploy**: set the env vars on the prod server and add a matching section to
   `docs/fafcn/how_to_deploy_fafcn_on_majiko.md`; verify the prod login round-trip at
   `https://faforever.cn:60`.
6. **Report back to FAF** that the integration works, and settle the remaining open
   items: test-environment client (test.faforever.com), `/me` shape confirmation, and
   whether extra scopes are possible later for stats/replay widgets.

### 2.2 Open question from Sheikah — gamedata mirror vs. ownership checks

Sheikah asked how the gamedata mirror interacts with FAF's ownership checks.
Current facts (verified in code):

- The mirror serves FAF patch archives (`env.nx2`, `units.nx2`, `textures.nx2`),
  map-generator jars, the downlords-faf-client installer, and community maps —
  **not** the base game and no SupCom executables.
- Files enter the mirror only from a trusted member's own legitimate FAF install
  (their official client downloaded them after their Steam-linked ownership check).
- Downloads are currently anonymous; there is **no ownership check on our side**.
  Mitigating context: FAF's own patch/CDN servers also serve these files without a
  per-download check — FAF enforces ownership at account registration (Steam link),
  and the patch files are useless without owning SupCom:FA anyway.
- Offer on the table if FAF prefers: gate gamedata downloads behind the same FAF
  OAuth login from this document, so only Steam-verified FAF account holders can
  download (auth middleware on the `ServeDir` mounts in
  `apps/fafcn-server/src/routes.rs`).

### 2.3 FAF feedback — the `faforever.cn` domain looks official (resolved 2026-08-30)

A FAF member raised that `faforever.cn` might make visitors think the site is
hosted by FAF ("We try to keep the faforever domain for only services that we
actually host"). We kept the domain (already registered, DNS/DDNS/cert all
wired) and instead made the non-official status unmistakable on the site:

- **Global footer** on every page: "community-run fan site, not hosted or
  endorsed by FAF" + link to `faforever.com` (`apps/fafcn-web/src/views/navbar.rs`).
- **Prominent banner on the home page** right below the hero with the same
  disclaimer (`apps/fafcn-web/src/views/home.rs`).

If FAF still finds the domain unacceptable, the fallback is re-registering a
clearly-unofficial domain (e.g. `fafcn.org`) and re-running the DNS/Lucky
cert steps from `how_to_deploy_fafcn_on_majiko.md` "Public domain" — the
client self-heals mirror addresses on new builds, so a domain switch is a
known, rehearsed operation.

---

## 3. Architecture overview

```text
Browser (Dioxus WASM SPA)                fafcn-server (Axum)            FAF
─────────────────────────                ───────────────────            ───
Click "Login with FAF" ────────────────> GET /api/auth/login
                                         302 ────────────────────────> /oauth2/auth
User authenticates on faforever.com
<──────────────────────────────────────  GET /api/auth/callback?code&state
                                         POST /oauth2/token ─────────> code exchange
                                         GET user info ─────────────> id + username
                                         create session row (SQLite)
<──────────────────────────────────────  302 /  +  Set-Cookie (HttpOnly)
SPA fetches /api/auth/me (cookie) ─────> { faf_username, role }
Upload screenshot ─────────────────────> POST /api/gallery  →  status = pending
Admin approves/features ───────────────> POST /api/gallery/:id/moderate
Every visitor's frontpage hero ────────> GET /api/gallery  →  featured images
```

- Identity = FAF player (`faf_user_id`, `faf_username`). Role is derived: `Admin` iff
  the FAF username is in `FAFCN_ADMIN_PLAYERS` (comma-separated env var,
  case-insensitive).
- No users table, no passwords, no password reset, no email. SQLite stores only
  **sessions** and **gallery items**.

---

## 4. Backend design (`apps/fafcn-server`)

Current state (verified): Axum 0.7, no DB, state = in-memory + filesystem, typed
`Error` enum (`apps/fafcn-server/src/error.rs`). Workspace already provides
`rusqlite 0.33 (bundled)`, `reqwest 0.12`, `uuid v4`. Image upload reuses the existing
raw-`Bytes` pattern (caption via query param) — no new multipart dependency.

### 4.1 SQLite schema (`src/db.rs`)

`Db = Arc<Mutex<rusqlite::Connection>>` (low traffic; KISS over a pool), opened with
`PRAGMA journal_mode=WAL`, idempotent schema, path from `FAFCN_DB_PATH`
(default `data/fafcn.db`):

```sql
-- docref: demo
CREATE TABLE IF NOT EXISTS sessions (
  token TEXT PRIMARY KEY,
  faf_user_id TEXT NOT NULL,
  faf_username TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS gallery_items (
  id TEXT PRIMARY KEY,
  filename TEXT NOT NULL,
  faf_user_id TEXT NOT NULL,
  faf_username TEXT NOT NULL,     -- attribution snapshot, not account data
  caption TEXT NOT NULL DEFAULT '',
  status TEXT NOT NULL CHECK (status IN ('pending','approved','rejected')),
  featured INTEGER NOT NULL DEFAULT 0,
  uploaded_at INTEGER NOT NULL
);
```

### 4.2 Auth module (`src/handlers/auth/`, thin `mod.rs`)

```text
handlers/auth/
  mod.rs       # index + re-exports
  types.rs     # Session, Role enum, Me DTO (typed serde structs, no Value)
  oauth.rs     # FafOAuthClient: authorize URL, code exchange, user info fetch
  store.rs     # SessionStore on Db (create/lookup/delete, lazy expiry sweep)
  extractor.rs # AuthUser / AdminUser FromRequestParts extractors
  handlers.rs  # HTTP handlers
```

Flow (server-side, `reqwest`):

1. `GET /api/auth/login` — random `state` (uuid, in-memory map, 10-min expiry) →
   `302` to `https://hydra.faforever.com/oauth2/auth?response_type=code&client_id=…
&redirect_uri=…&scope=public_profile&state=…`.
2. `GET /api/auth/callback?code&state` — verify state → POST code exchange to
   `https://hydra.faforever.com/oauth2/token` (form-encoded, client_id + secret) →
   GET user info with the access token (`https://api.faforever.com/me` — validated
   during implementation; extract FAF
   id + login name) → create session (uuid token, 30 days) →
   `Set-Cookie: fafcn_session=<token>; HttpOnly; Path=/; SameSite=Lax; Max-Age=…` →
   `302` to `FAFCN_PUBLIC_BASE_URL` (`/` in prod).
3. `POST /api/auth/logout` — delete session, clear cookie.
4. `GET /api/auth/me` — `{ faf_username, role }`; 401 without a valid session.

Extractors: `AuthUser` (cookie → session, else 401), `AdminUser` (`Role::Admin`,
else 403). OAuth endpoints/config overridable via env so the FAF test environment can
be pointed at instead.

### 4.3 Gallery module (`src/handlers/gallery/`, thin `mod.rs`)

```text
handlers/gallery/
  mod.rs       # index + re-exports
  types.rs     # GalleryItem + GalleryStatus enum + ModerationAction enum + DTOs
  store.rs     # GalleryStore: items table on Db + image files on disk
  handlers.rs  # HTTP handlers
```

State as enum (per project `AGENTS.md`), strings only at the DB boundary:

```rust
// docref: demo
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum GalleryStatus {
    Pending,
    Approved,
    Rejected,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ModerationAction {
    Approve,
    Reject,
    SetFeatured { featured: bool },
}
```

- Files live in `FAFCN_GALLERY_DIR/files/` (default `data/fafcn-gallery`), named
  `<uuid>.<ext>` — extension from sniffed type only, never user input.
- Validation: `image/jpeg|png|webp` verified by magic bytes; body limit 8 MB;
  caption ≤ 140 chars, trimmed, optional.

| Method & path                    | Auth        | Purpose                               |
| -------------------------------- | ----------- | ------------------------------------- |
| `GET /api/gallery`               | public      | **approved** items only, newest first |
| `POST /api/gallery?caption=..`   | `AuthUser`  | raw image bytes → `Pending`           |
| `GET /api/gallery/admin`         | `AdminUser` | all items incl. pending/rejected      |
| `POST /api/gallery/:id/moderate` | `AdminUser` | typed `ModerationAction` JSON         |
| `DELETE /api/gallery/:id`        | `AdminUser` | delete row + file                     |
| `GET /api/gallery/files/*`       | public      | `ServeDir` static mount               |

Public DTO: `{ id, url, uploader, caption, featured, uploaded_at }`.

### 4.4 Wiring & config changes

- `config.rs`: `FAFCN_DB_PATH`, `FAFCN_GALLERY_DIR`, `FAFCN_OAUTH_CLIENT_ID`,
  `FAFCN_OAUTH_CLIENT_SECRET`, `FAFCN_PUBLIC_BASE_URL` (default `/`),
  `FAFCN_ADMIN_PLAYERS`.
- `state.rs` / `main.rs` / `routes.rs`: open `Db`, build stores, mount routes
  (8 MB body limit only on the upload route).
- **CORS**: `allow_origin(Any)` is invalid with credentials; change to an explicit
  localhost dev-origin list + `allow_credentials(true)`. Production is same-origin
  via nginx and unaffected.
- `error.rs`: add `Forbidden` → 403.

---

## 5. Frontend design (`apps/fafcn-web`)

Current state (verified): Dioxus 0.7 WASM SPA, precompiled Tailwind v4, gloo-net REST,
hand-rolled En/Zh i18n enum. The home hero is a single inline `background-image` div
(`apps/fafcn-web/src/views/home.rs ~line 21 — Home`).

### 5.1 Auth state (`src/auth.rs`)

```rust
// docref: demo
pub enum AuthState {
    Loading,
    Anonymous,
    LoggedIn(Me), // Me { faf_username, role: Role }
}
```

Provided once at app root (`use_provide_auth`, alongside `use_provide_lang`) by
fetching `/api/auth/me` with `credentials: include`. Navbar: anonymous →
**"Login with FAF"** button (full-page redirect to `/api/auth/login` — it bounces
through faforever.com and back); logged in → FAF username (+`★` for admin) + Logout.
No local login/register pages at all.

### 5.2 Gallery page (`src/views/gallery.rs`, route `/gallery`)

- Public responsive grid of approved items (image `object-cover`, uploader FAF name,
  caption, amber "featured" badge), click → lightbox.
- Upload card only when `LoggedIn` (anonymous → "log in with FAF to share" prompt);
  file input + caption → POST raw bytes; success → "submitted, awaiting approval".
- Admin section only when `role == Admin`: Pending list (thumbnail + uploader +
  Approve/Reject), Approved list (★ feature toggle + Delete), re-fetch after each
  action.

### 5.3 Living hero slideshow (`src/components/hero_slideshow.rs`)

Replaces the static hero div in `Home`:

- Fetch `GET /api/gallery`, keep `featured` entries; empty/loading/error → render the
  current static hero **unchanged** (fallback).
- Stacked `absolute inset-0` slide layers (`background-size: cover`); active slide
  `opacity-100`, others `opacity-0`, `transition-opacity duration-[1500ms]`; the active
  slide's inner layer runs injected CSS keyframes `scale(1.0) → scale(1.12)` over ~8 s
  (keyframes via Dioxus `document::Style`, since Tailwind CSS is precompiled).
- Advance with a spawned timer loop; existing gradient overlay + headline/CTAs stay on
  top untouched; small `📷 {uploader}` attribution bottom-right fading with the slide.

### 5.4 i18n

~20 new `Text` variants (En + Zh, compiler-enforced): nav labels, gallery title,
featured badge, upload labels/success/error, login prompt, admin labels.

---

## 6. Environment variables (deployment)

| Var                         | Purpose                             | Default              |
| --------------------------- | ----------------------------------- | -------------------- |
| `FAFCN_DB_PATH`             | SQLite file (sessions + gallery)    | `data/fafcn.db`      |
| `FAFCN_GALLERY_DIR`         | gallery image storage               | `data/fafcn-gallery` |
| `FAFCN_OAUTH_CLIENT_ID`     | FAF OAuth client id                 | unset → login 503    |
| `FAFCN_OAUTH_CLIENT_SECRET` | FAF OAuth client secret             | unset → login 503    |
| `FAFCN_PUBLIC_BASE_URL`     | post-login redirect target          | `/`                  |
| `FAFCN_ADMIN_PLAYERS`       | comma-separated admin FAF usernames | none                 |

(Deployment doc `docs/deploy_fafcn/how_to_deploy_fafcn.md` gets a matching section at
implementation time.)

---

## 7. Rollout phases

1. **FAF contact** — ✅ done: application approved (section 2), credentials pending.
2. **OAuth round-trip** — ⏳ blocked on credentials: verify registration, implement
   the auth module (4.2), local smoke test, prod smoke test (section 2.1 checklist).
3. **Gallery implementation** — blocked on phase 2: backend gallery (4.3), frontend
   gallery page + hero slideshow (5.2–5.3).
4. **Production announcement** — announce in the QQ group, seed the first featured
   images ourselves.

## 8. Out of scope (future increments)

- Likes/comments, per-user gallery pages, FAF avatar/rating on gallery cards.
- Server-side thumbnails (originals served directly; grid uses `object-fit: cover`).
- Upload rate limiting (add per-user/IP throttling if abuse appears).
- Refresh-token renewal (sessions are re-created by logging in again after 30 days).
