# Debt: monday.com OAuth sign-in

monday.com signs in with a **personal API token** today (Settings → Integrations →
monday.com). Each teammate pastes their own token, so writes show under their
name. The token lives in the OS credential store; Disconnect deletes it.

## Why OAuth is not there yet

monday's OAuth 2.1 flow requires PKCE **and** a `client_secret` at the token
endpoint (`token_endpoint_auth_methods_supported: ["client_secret_post"]`). A
desktop app cannot keep a secret, so OAuth needs a small broker that holds it.

## What paying it off takes

1. Create a monday app in the Developer Center, turn on "New OAuth Flow", pick
   scopes: `me:read boards:read boards:write updates:read updates:write
   workspaces:read users:read`. Share it (Distribute → Share app) so other
   accounts can install it.
2. Deploy a broker (a Cloudflare Worker): `/start` redirects to
   `https://auth.monday.com/oauth2/authorize` with PKCE; `/callback` exchanges the
   code with the secret, then redirects to `http://127.0.0.1:<port>/done` with
   the tokens. Access tokens expire (read the JWT `exp`); refresh tokens rotate
   and are single-use.
3. In the app: a "Sign in with monday.com" button in Settings → Integrations
   that opens the broker and listens on a loopback port, and a refresh step in
   `integrations::connect`. Everything above `Tracker` stays as it is.

Personal tokens stay as the fallback.
