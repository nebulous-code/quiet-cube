# Milestone 8 — Operator Stats

Scope: a once-a-week email digest that tells the operator (me) whether the user base went up, down, or sideways, plus the absolute counts. Replaces the "Admin panel" idea from Post-MVP with something dramatically cheaper to build and maintain. No new product surface — entirely an operator/observability concern.

---

## 1. Goal recap

By the end of M8:

- A scheduled GitHub Action runs weekly against prod Neon read-only and emails a digest to the operator inbox.
- The digest answers the only question I actually have today: "is anyone using this, or is it just me?" Specifically: total user count right now, verified vs unverified split, new signups this week, deletions this week, net delta, the same numbers for last week for trend, and a multi-week sparkline of net deltas going back as far as the history file extends.
- Current-week figures are computed from `users.created_at` and `account_deletions.deleted_at` at query time. Multi-week history lives in a committed `tools/operator_stats_history.json` that the workflow appends to and pushes back to `main` after each successful run.
- The query path uses a **SELECT-only Neon role** so a leaked `STATS_DATABASE_URL` secret can't write or read sensitive columns. The role grants `SELECT` on aggregates and on safe columns of `users` and `account_deletions`; never on `password_hash`, `email`, etc.
- The email is delivered via the existing Resend integration (same domain, same `RESEND_API_KEY`, same `Quiet Cube <noreply@mail.nebulouscode.com>` sender). A `workflow_dispatch` trigger lets me run it manually for sanity-checks without waiting for cron.
- If the script fails (DB unreachable, query error, JSON write error, etc.) it catches the error, sends a "Stats run failed: <message>" email to the operator via Resend, and exits non-zero so GitHub's own failure notification also fires. Belt-and-suspenders against silent breakage.
- Workflow logs and the committed history file are aggregate-only — they never contain emails, user IDs, or any PII.

Out of scope (deferred — tracked under Post-MVP in `docs/POST_MVP.md` if revisited):

- **Active-users metric ("DAU/WAU").** Needs a `users.last_seen_at` column that doesn't exist today; the "Reduce session cookie lifetime / add idle expiry" Post-MVP item would introduce it. Punt until that lands.
- **Discord / Slack / other delivery channels.** Email is the inbox I actually check. Alternate destinations are a per-channel `curl` away if I change my mind later.
- **A full admin UI.** Same reasoning as the original Post-MVP cut: a couple of `SELECT count(*)` queries don't justify a built surface.

---

## 2. Architecture for M8

This milestone has no Rust, no Vue, no migrations against the app schema — only ops glue.

### Pieces

- **`.github/workflows/operator-stats.yml`** — scheduled (weekly cron) + `workflow_dispatch`-triggerable workflow. Runs in a `production` GitHub environment (see §8) so the secrets are scoped, not just bare repo secrets. Sets up Node, runs the script with secrets injected, commits the updated history file back to `main`, fails loudly on any non-zero exit so GitHub's own failure-notification emails serve as the dead-man switch.
- **`tools/operator_stats.js`** — the script. Node ≥18 (uses built-in `fetch`), depends on `pg` only. Connects to Neon read-only, runs the aggregate queries (§4), reads and appends to `tools/operator_stats_history.json`, formats the digest, posts to Resend. Catches its own errors and sends a failure email before re-throwing.
- **`tools/operator_stats_history.json`** — committed file. One entry per successful run with the week's aggregates. Drives the sparkline in the digest. Grows by ~52 entries per year; harmless to version-control forever.
- **Neon read-only role** — a new role provisioned manually in the Neon console with `SELECT` granted on the columns/tables the script needs. Its connection string lives in the `production` environment secret store as `STATS_DATABASE_URL` (distinct from `DATABASE_URL` to make the "this is a different blast radius" obvious).
- **Resend integration reuse** — the same Resend account that ships verification email, same `RESEND_API_KEY`, same verified `Quiet Cube <noreply@mail.nebulouscode.com>` sender. The script POSTs to `https://api.resend.com/emails` with the operator address as the recipient. No new vendor, no new DNS, no new sender to verify.

### Reuse from M1–M7

- Resend client setup, DKIM/SPF/DMARC, and the sender domain are all already in place from M1.
- `users.created_at` (M1, migration 0001) and `account_deletions.deleted_at` (M7, migration 0007 / refined in 0012) are the only columns the script needs.
- The "secret stays in one place" pattern follows the same logic as `JWT_SECRET` and `ACCOUNT_DELETION_HMAC_SECRET` — a single source of truth, never duplicated across services.

### What this milestone deliberately does not touch

- No changes to the app schema. The new Neon role is a permission grant, not a migration.
- No app code changes. No new endpoints, no Pinia stores, no Vue views.
- No new vendor. Everything runs on GitHub Actions, Neon, and Resend, all of which we already use.

---

## 3. Schema — M8 changes

No app-schema migration. The change is at the Postgres role / privilege layer, applied manually via the Neon SQL console (or a one-shot script in `tools/`):

```sql
-- Run once, manually, against the prod Neon project.
CREATE ROLE stats_readonly LOGIN PASSWORD '<generated>';

-- Aggregate-friendly grants, no PII columns.
GRANT CONNECT ON DATABASE <db_name> TO stats_readonly;
GRANT USAGE  ON SCHEMA public        TO stats_readonly;
GRANT SELECT (id, created_at, email_verified) ON users              TO stats_readonly;
GRANT SELECT (id, deleted_at)                 ON account_deletions  TO stats_readonly;
```

Notes:

- Column-level `GRANT SELECT (...)` keeps `password_hash`, `email`, `display_name`, and the rest of `users` invisible to this role. Even an attacker who exfils the GitHub secret can only count rows, not read identities.
- The role intentionally has no `INSERT`, `UPDATE`, `DELETE`, or `TRUNCATE` privileges anywhere. The connection string is a read-only window.
- Dev gets the same treatment so the dev workflow run mirrors prod behavior.

---

## 4. Stats surface — what the digest contains

The script issues one query per logical figure (or one combined CTE — implementation detail). Conceptually:

| Figure | Source | Notes |
|--------|--------|-------|
| Total users right now | `SELECT count(*) FROM users` | The single absolute number |
| Verified vs unverified | `GROUP BY email_verified` | Surfaces a silently broken verification-email path |
| New signups, this week | `users.created_at >= now() - interval '7 days'` | Counts only |
| New signups, last week | `users.created_at >= now() - interval '14 days' AND created_at < now() - interval '7 days'` | For the comparison |
| Deletions, this week | `account_deletions.deleted_at >= now() - interval '7 days'` | |
| Deletions, last week | `account_deletions.deleted_at >= now() - interval '14 days' AND deleted_at < now() - interval '7 days'` | |
| Net delta, this week | signups_this_week − deletions_this_week | Derived |
| Trend arrow | delta_this_week vs delta_last_week | `↑ / → / ↓` |
| Sparkline (last N weeks) | from `tools/operator_stats_history.json` | ASCII bar chart of net delta per week |

The email body looks roughly like this (final copy can be tweaked in implementation):

```
Quiet Cube — week of 2026-05-13

Total users:        47       (37 verified, 10 unverified)
  Net this week:    +5
  New signups:      6
  Deletions:        1

Last week:          +3 net
  New signups:      4
  Deletions:        1

Trend: up ↑

Net delta, last 6 weeks
  -2w  ▁
  -1w  ▂ ▂ ▂
  now  ▅ ▅ ▅ ▅ ▅
```

The sparkline truncates gracefully — on the first run (one data point) it just shows `now`; by week 6 you get a real shape.

---

## 5. Workflow + script — M8

### Workflow file

`.github/workflows/operator-stats.yml`:

- **Triggers:** `schedule: cron: '0 13 * * 1'` (Mondays 13:00 UTC = 9 AM ET) plus `workflow_dispatch` for manual runs.
- **Environment:** `environment: production` so the secrets resolve from the `production` GitHub environment, not bare repo secrets.
- **Permissions:** `contents: write` — the workflow commits an updated `tools/operator_stats_history.json` back to `main` after each successful run. Nothing else gets write scope.
- **Job:** single job on `ubuntu-latest`. Steps: checkout, set up Node 20, `npm install pg` (or use `npm ci` if we adopt a lockfile), run `node tools/operator_stats.js` with `STATS_DATABASE_URL`, `RESEND_API_KEY`, and `OPERATOR_EMAIL` injected. On success, configure git as a bot identity (`github-actions[bot]`), commit the updated history file with a `[skip ci]` message, and push. Exits non-zero on any failure; GitHub's failure notification + the in-script Resend failure email together serve as the alarm.
- **Concurrency:** `concurrency: operator-stats` with `cancel-in-progress: false`. Belt-and-suspenders against accidental double-runs.

### Script

`tools/operator_stats.js`:

- Reads env: `STATS_DATABASE_URL`, `RESEND_API_KEY`, `OPERATOR_EMAIL`, optional `EMAIL_FROM` (falls back to `Quiet Cube <noreply@mail.nebulouscode.com>`).
- Opens one connection to Neon, runs the aggregate queries, closes the connection.
- Reads `tools/operator_stats_history.json` (create-if-missing). Appends one entry for this week: `{ "week_of": "2026-05-13", "total_users": 47, "verified": 37, "unverified": 10, "signups_7d": 6, "deletions_7d": 1, "net_7d": 5 }`. Writes back.
- Formats the digest body (plain text — keep simple; HTML if cheap in the chosen language).
- POSTs to `https://api.resend.com/emails` with `from`, `to`, `subject`, `text` (and optionally `html`).
- Logs to stdout: "Stats: total=47 signups_7d=6 deletions_7d=1" — aggregates only, no PII. Logs go to the GitHub Actions UI which is visible to repo collaborators.
- Wraps the entire flow in a try/catch. On error: log the error, attempt a fallback "Stats run failed: <message>" Resend send (best-effort — wrapped in its own try/catch so a Resend outage doesn't mask the original error), then `process.exit(1)`. GitHub's workflow-failure notification fires regardless; the Resend failure email is the additional belt that arrives in the same inbox as the normal digest.

### What's already done

- The Resend account + verified sender domain.
- `users.created_at`, `account_deletions.deleted_at` columns.

---

## 6. Security notes specific to M8

- **Read-only Neon role.** The script's connection string can `SELECT` a handful of columns across two tables. Cannot read `users.email`, cannot read `users.password_hash`, cannot write anything. A leak of `STATS_DATABASE_URL` is recoverable by rotating the role's password.
- **No PII in logs or in the committed history file.** Queries return aggregate counts only. The script prints aggregates to stdout and persists aggregates to `tools/operator_stats_history.json`. The Actions UI and the history file are both visible to anyone with repo read access; this matters even for a solo repo because GitHub's own employees and any future collaborator inherit that visibility. Code review on this script should reject any non-aggregate column the same way a security review would.
- **Email recipient as a secret, not a hardcoded literal.** `OPERATOR_EMAIL` lives in the `production` environment secret store so the workflow file is publishable without exposing the address.
- **Secrets blast radius.** Three secrets total: `STATS_DATABASE_URL` (read-only, easily rotatable), `RESEND_API_KEY` (shared with the app — rotating it is a coordinated app+workflow operation), `OPERATOR_EMAIL` (low sensitivity).
- **`workflow_dispatch` exposure.** Anyone with write access to the repo can trigger the workflow manually. For a solo project this is fine; revisit if collaborators get added.
- **`contents: write` scope.** Required for the workflow to commit the updated history file back to `main`. The token is the default `GITHUB_TOKEN`, scoped to this single workflow's job, and never persisted. The commit message includes `[skip ci]` to keep the stats commit from re-triggering any deploy pipelines listening on `main`.
- **`production` GitHub environment.** Scopes the three secrets to this single environment so any future workflow that hasn't been opted in cannot read them. The environment is also configured to restrict secret access to the `main` branch only, so a feature-branch fork of the workflow can't exfiltrate the secrets via a malicious PR.

---

## 7. Testing strategy

This milestone has no Rust unit tests and no Vue component tests. Verification is end-to-end and manual.

**Dry-run sequence:**

1. Run the script locally against the dev DB (using a dev-side read-only role) with the operator email pointed at a personal inbox. Confirm the digest renders, the numbers look sane, and Resend delivers.
2. Push the workflow file. Use `workflow_dispatch` to trigger a run against prod with the real `STATS_DATABASE_URL`. Confirm the prod digest arrives.
3. Let the cron fire on its scheduled day. Confirm the scheduled run also arrives (catches issues that only show up under the cron trigger context — e.g. secret scope, schedule timing).
4. Force a failure (point `STATS_DATABASE_URL` at a non-existent role) and confirm GitHub sends the workflow-failure notification email. The script's exit code is the dead-man switch.

**Defensive cases worth a one-time check:**

- Empty DB (or DB where both weeks have zero signups/deletions). The trend math shouldn't divide by zero or render "NaN".
- Newly-launched-this-week scenario (no "last week" data — `last_week` numbers are zero). The trend line should still read sensibly.
- Neon free-tier sleep — the workflow waking a cold DB adds 10–30s. Workflow timeout (default 6 hours) is comfortably above this.

---

## 8. Configuration / environment

**`production` GitHub environment.** All three secrets below live inside a `production` GitHub environment (Repo Settings → Environments → New environment → `production`), not as plain repo secrets. The environment is configured to restrict secret access to the `main` branch only, so the workflow can only resolve these secrets when running from `main` — protects against a fork PR copying the workflow file and triggering a run that exfiltrates the secrets.

| Secret | Purpose | Notes |
|--------|---------|-------|
| `STATS_DATABASE_URL` | Read-only Neon connection string for the `stats_readonly` role | Distinct from prod `DATABASE_URL`. Document rotation procedure. |
| `RESEND_API_KEY` | Sends the digest email | Same value as the backend's `RESEND_API_KEY`. Rotating it requires updating both. |
| `OPERATOR_EMAIL` | Recipient address | Plain string, currently `me@nebulouscode.com`. |

**No new `.env.example` entries** — the script is workflow-only, not part of the app runtime. Local dry-runs read values from a developer's shell env, not from `backend/.env`.

### Setup walkthrough — first-time provisioning

One-time steps to get the workflow ready to run. Run in order; each builds on the previous.

**A. Create the read-only Neon role (prod).**

1. Open the prod Neon project → SQL Editor.
2. Generate a fresh password locally: `openssl rand -hex 32` (or the PowerShell equivalent used for `JWT_SECRET`).
3. Paste the §3 `CREATE ROLE` + `GRANT` block, replacing `<generated>` with that password and `<db_name>` with the actual prod database name. Run it.
4. Verify with a quick connection: open the Neon connection-string panel, swap the username and password to `stats_readonly:<your-password>`, copy that string. From a local shell:
   ```
   psql "<the-string>" -c "SELECT count(*) FROM users"     # succeeds
   psql "<the-string>" -c "SELECT email FROM users LIMIT 1" # fails: permission denied for column email
   psql "<the-string>" -c "INSERT INTO users (email) VALUES ('x')" # fails: permission denied
   ```
   The two failures confirm the role is properly fenced.
5. Save the working connection string — this is the value for `STATS_DATABASE_URL` in step C.

**B. Repeat for the dev Neon project.** Same role name, different password. Saves a separate `STATS_DATABASE_URL` value to use for dry-runs.

**C. Create the `production` GitHub environment.**

1. Repo page → **Settings** (top nav) → **Environments** (left sidebar) → **New environment**.
2. Name it `production`. Click **Configure environment**.
3. Under **Deployment branches and tags**, switch from "No restriction" to **"Selected branches and tags"**. Click **Add deployment branch or tag rule** → enter `main` (exact match, no wildcards). Click **Add rule**. This is the bit that prevents a fork PR from copying the workflow file and triggering a run that exfiltrates the secrets.
4. **Skip "Required reviewers"** for this environment. Required reviewers add a manual-approval gate before every run, including the cron — perfect for "deploy to prod" workflows but wrong for an unattended weekly digest. (Worth knowing the option exists for future high-stakes workflows.)
5. **Skip "Wait timer"** for the same reason — useful for staggered deploys, not for a fire-and-forget digest.
6. Under **Environment secrets**, click **Add secret** three times:
   - `STATS_DATABASE_URL` — the prod connection string from step A.5.
   - `RESEND_API_KEY` — same value as the backend service. Copy from Render → backend service → Environment.
   - `OPERATOR_EMAIL` — `me@nebulouscode.com`.

**D. Confirm the workflow can see the environment.**

After the workflow file lands on `main` (per §11 stories), trigger a manual run:

1. Repo page → **Actions** → **Operator Stats** workflow → **Run workflow**.
2. The first run is the smoke test: confirm the email lands in `OPERATOR_EMAIL`, the workflow log shows the aggregate-only line, and `tools/operator_stats_history.json` has gained one entry committed by `github-actions[bot]`.

**Rotation procedure** (when needed later):

- *Rotate the Neon role password*: in Neon SQL Editor, `ALTER ROLE stats_readonly WITH PASSWORD '<new>'`. Update the `STATS_DATABASE_URL` secret in the `production` environment. No app-side coordination.
- *Rotate `RESEND_API_KEY`*: must be done in lockstep with the backend service since both share the key. Update Resend dashboard → both the Render backend env var and the GitHub environment secret.
- *Change `OPERATOR_EMAIL`*: edit the environment secret directly; takes effect on the next run.

---

## 9. Migration / data risk

No DB migration. Behavior risk is operational, not data:

- **Forgotten secrets / rotation drift.** If `RESEND_API_KEY` is rotated for the app but not for the workflow, the digest silently breaks. Mitigation: rotate both in lockstep; the dead-man failure email surfaces it within a week.
- **Scheduled-cron skew.** GitHub Actions cron is best-effort and can be delayed by tens of minutes during platform load. Acceptable for a weekly digest.
- **Neon role drift.** A future app migration that renames `users.created_at` or `account_deletions.deleted_at` breaks the stats query but not the app. The dead-man failure email is again the signal.
- **PII leak via accidental script edit.** A future change that adds `SELECT email FROM users` to the script would surface emails in workflow logs. Code review on this script should treat any non-aggregate column the same as a security review.

---

## 10. "Done when" checklist (run on the deployed instance)

A literal QA script. M8 closes when every line passes.

- [ ] `stats_readonly` role exists on prod Neon with the §3 grants. Connecting with it and running `SELECT 1 FROM users LIMIT 1` succeeds. Running `INSERT INTO users (...)` or `SELECT email FROM users` fails with a permission error.
- [ ] Same role exists on dev Neon with the same grants.
- [ ] `production` GitHub environment exists with `STATS_DATABASE_URL`, `RESEND_API_KEY`, `OPERATOR_EMAIL` populated and access restricted to the `main` branch.
- [ ] `.github/workflows/operator-stats.yml` is on `main` and visible in the Actions tab.
- [ ] `workflow_dispatch` run against prod completes green, the resulting email lands in `OPERATOR_EMAIL` with non-empty numbers and the verified/unverified line, and `tools/operator_stats_history.json` gains one new entry committed by `github-actions[bot]` with a `[skip ci]` message.
- [ ] Scheduled cron has fired at least once successfully and delivered an email.
- [ ] A deliberate failure run (point at a bad secret) triggers *both* GitHub's workflow-failure notification email *and* the script's "Stats run failed" Resend email.
- [ ] Workflow logs and `operator_stats_history.json` contain only aggregate numbers — no email addresses, no user IDs, no PII.
- [ ] Sparkline renders gracefully on the first run (one entry → just "now" rendered) and on a multi-week history (≥3 entries → real shape).
- [ ] The "active in last 7 days" metric is deliberately absent (it requires schema work scoped out of this milestone — see §1).

---

## 11. Story list

No frontend or app-backend stories. Renamed prefixes for clarity: **I** (infrastructure), **S** (script), **Q** (QA).

- [ ] **I1.** Provision `stats_readonly` role on prod Neon per §3. Repeat on dev Neon for parity.
- [ ] **I2.** Create the `production` GitHub environment, restrict to `main` branch, populate `STATS_DATABASE_URL`, `RESEND_API_KEY`, `OPERATOR_EMAIL` secrets inside it.
- [ ] **S1.** `tools/operator_stats.js` — connects via `pg`, runs aggregate queries (incl. verified/unverified split), reads/writes `operator_stats_history.json`, formats text body with sparkline, POSTs to Resend. Catches errors and sends failure-mode email before re-throwing. Reads all sensitive inputs from env.
- [ ] **S2.** `.github/workflows/operator-stats.yml` — `schedule` (Monday 13:00 UTC) + `workflow_dispatch`, `environment: production`, `contents: write` permission, single `ubuntu-latest` job, Node 20, runs S1 then commits the updated history file with `[skip ci]`. Concurrency set to single-run.
- [ ] **S3.** `tools/operator_stats_history.json` — seeded as `[]` so the first run has a place to append into.
- [ ] **Q1.** Dry-run locally against dev DB; capture the rendered email body in a screenshot for the docs.
- [ ] **Q2.** Walk §10 against the deployed workflow, including the deliberate-failure check.

---

## 12. Decisions (resolved)

1. **Script language. Node.** Built-in `fetch` on Node ≥18; only third-party dep is `pg`. Matches the repo's existing Node toolchain.
2. **Cron schedule. `'0 13 * * 1'`** — Monday 13:00 UTC = 9 AM ET. Monday-morning recap.
3. **Resend API key scoping. Reuse the prod `RESEND_API_KEY`.** One secret, one rotation. Documented that any rotation is a coordinated app+workflow operation.
4. **Email sender. Reuse `Quiet Cube <noreply@mail.nebulouscode.com>`.** Same sender as the user-facing verification email; no new sender to verify.
5. **Recipient. `me@nebulouscode.com`** as a single address in `OPERATOR_EMAIL`.
6. **Multi-week history. Yes — `tools/operator_stats_history.json` committed back to `main`** after each successful run. The workflow takes `contents: write` for this single commit step, message includes `[skip ci]`, the script appends one entry per run. Enables a real sparkline from week ~3 onward; the digest copes gracefully with shorter histories.
7. **Script location. `tools/operator_stats.js`** + workflow in `.github/workflows/operator-stats.yml`. Script is discoverable alongside other operator tooling; workflow lives where GitHub expects it.
8. **`production` GitHub environment. Yes.** All three secrets are scoped to the `production` environment with branch protection limiting access to `main`. Setup walkthrough: see implementation notes in the PR description.
9. **Verified vs unverified split. Yes** — surfaces a silently broken verification-email path. Renders as a single annotation on the total-users line: `Total users: 47 (37 verified, 10 unverified)`.
10. **Failure-mode delivery. Both.** Script catches its own errors, attempts a "Stats run failed: <message>" Resend send, exits non-zero. GitHub's workflow-failure notification fires in addition. Two notifications on failure, single notification on success.

---

## 13. Looking back

A retrospective pass added after the milestone shipped. Fill in honestly — the value of these notes is the friction they capture, not a victory lap.

- **What I'd do differently if I started this milestone today:**
    - Step A-4 needs to exercise both tables, not just users. A select from information_schema would've made sure I had the right columns available.

- **Surprises during execution:**
    - Deadman failure path sent me an email about the failure which helped me fix the table permission above. Paid for itself on the implementation.

- **Decisions that turned out to matter more than expected:**
    - putting secrets behind a production github environment instead of as a repo secret made the setup easier and may save time on onboarding a new collaborator.
    - Filure mode email helped (see above)

- **Decisions that turned out not to matter:**
    - Probably could've given read only user more column permissions to reduce copy paste typos. 
    - Over all everything went smoothly.

- **What this milestone taught me that I'd carry into future work:**
