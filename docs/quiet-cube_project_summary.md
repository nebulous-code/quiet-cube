# Quiet-Cube
<!-- One sentence summary, no jargon. What it is, for someone who will never see the code. -->

A website to help people learning to solve the Rubik's cube faster. It keeps track of the moves you've recently learned and reminds you to practice them before you forget.

**Status:** Shipped

**Released:** May 2026

## Why I Built It

<!-- Two or three sentences about the problem in human terms focused on me (I usually scratch my own itch) or someone I'm building it to help. What frustrated me enough to write code for it. What was solving the need in a way that fell short. What interested me in the puzzle or project. If it's just something I was playing around with mention that as well. -->

I've been cubing for over five years, sitting around the 60-second mark, and the thing standing between me and going faster is OLL — a step of the CFOP method with 57 separate algorithms to memorize. My practice system was a stack of index cards with each pattern drawn on the front and my algorithm on the back, flipped through with a solved cube in hand: it worked, but it was slow and messy, and the cards had no idea which cases I actually kept forgetting. I wanted the same physical loop with something else keeping track of what to show me next, so the limited practice time I have goes to the algorithms I haven't locked in yet.

## What It Does

<!-- High level bullet points of its capabilities, in plain language. At least three if you can come up with them:
     - a plain language capability
     - another plain language capability
     - one more -->

- Shows you a cube pattern, waits while you run the algorithm on a real cube in your hands, then reveals the answer so you can check your cube against the picture
- Lets you grade yourself honestly on a four-button scale, and uses that to decide when each case should come back — weak ones sooner, solid ones out of the way
- Gives you a browsable reference of all 57 cases, filterable by shape and by how well you know them, where you can swap in your own algorithm if you don't like the default
- Tracks where you stand: how many cases are untouched, in progress, due, or locked in, plus a daily practice streak
- Runs the whole loop without an account if you'd rather not sign up, and folds that practice history into a real account if you change your mind later

## Who It's For

<!-- A quick outline of who is or would want to use it. -->

Cubers who already solve with CFOP and have hit the OLL wall — the point where the next real speed gain means memorizing a lot of algorithms and keeping them memorized. It assumes you practice with a cube physically in front of you and want to be left alone while you do it: no timer, no leaderboard, no opponents, no ads, no trackers. Mostly it's for me, but the shape of that problem isn't unique to me.

## How It Works

<!-- A technical summary for those who want to know. -->

A Rust API serves a Vue single-page app. Case data and per-user scheduling state live in Postgres, and each graded answer runs through an Anki-flavored variant of the SM-2 spaced-repetition algorithm to compute when that case is next due. Accounts are email plus password with verification codes, and sessions are HS256 JWTs in httpOnly cookies that carry a session id, so revocation is a database lookup rather than a token you can only wait out. Guest mode keeps the same data shapes in localstorage so the study loop never needs the network until you decide to make an account.

- **Built with:** Rust (Axum, sqlx) on the backend; Vue 3, TypeScript, and Vite on the frontend
- **Runs on:** Render (API service plus static site), with Postgres hosted on Neon
- **Talks to:** Resend for transactional email (verification and password-reset codes), and Cloudflare Turnstile for signup bot protection
- **Leverages:** sqlx for SQL checked against the real schema at compile time, argon2 for password hashing, jsonwebtoken for the session cookies, and Pinia for frontend state; the cube pattern diagrams are hand-rolled SVG rather than a charting library

## What It Taught Me

<!-- A high level summary of what I learned building it. -->

The lesson that keeps resurfacing is that small early decisions get load-bearing fast. Columns I added defensively before I needed them cost nothing at creation and saved migrations later. Choosing to merge user overrides onto default case data in SQL rather than in application code made a schema rework much cheaper when it came. And a session cookie choice I made because it looked obviously safe turned out to constrain how I can build a dev environment and a future mobile app — a decision I'll probably have to undo, made in about a minute.

The other half is about testing and honesty. I built the database test harness early and it paid for itself across every milestone after, but I kept deferring an end-to-end browser test — and launch day surfaced bugs that a single smoke test would have caught. Backend coverage sitting near 98% said nothing about whether the page actually loaded. I also learned that some decisions I agonized over never mattered at all, which is only visible in hindsight and only if you write the retrospective down while it's fresh.

