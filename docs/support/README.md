# The support page

`site/` is the support page at <https://jrssae.github.io/sparkamp/>, the
Support URL in App Store Connect. It is plain HTML with no build step, and
`.github/workflows/pages.yml` publishes it whenever `site/` changes on `main`.

A static page cannot send mail or post an issue, so each of its two choices
hands the visitor's words to a service that can, filled in and ready to send:

- **I'm reporting a bug** opens GitHub's new-issue page with the bug-report
  form (`.github/ISSUE_TEMPLATE/bug_report.yml`) filled in, field by field.
  The page's field names are the issue form's ids; rename one and change the
  other. Posting needs a GitHub account, and the page says so.
- **I need help** opens a Google form with the visitor's name, email and
  message filled in. Submitting it needs no account. An Apps Script on the
  form emails each request to you.

Nothing is sent until the visitor presses Create or Submit on the other site.

## One-time setup

### 1. Turn on GitHub Pages

Repository → Settings → Pages → Build and deployment → Source: **GitHub
Actions**. The next push to `main` that touches `site/` publishes the page;
Actions → Support site → Run workflow publishes it at once.

### 2. Create the help form

At <https://forms.google.com>, start a blank form called "Sparkamp support"
with exactly these three questions, in this order:

| Question | Type | Required | Notes |
|---|---|---|---|
| Your name | Short answer | No | |
| Your email | Short answer | Yes | ⋮ → Response validation → Text → Email |
| How can we help? | Paragraph | Yes | |

In the form's Settings → Responses, leave **Collect email addresses** off and
**Limit to 1 response** off. Either would make visitors sign in to Google.

### 3. Connect the page to the form

In the form, ⋮ → **Get pre-filled link**. Type `NAME`, `EMAIL` and `MESSAGE`
(in capitals, exactly) into the three answers, press **Get link**, then
**Copy link**. Paste it into `site/config.js` as `helpFormPrefill`.

The page swaps those three words for what the visitor typed, so the link
needs nothing else done to it.

### 4. Get each request by email

In the form, ⋮ → **Apps Script**. Replace the editor's contents with
[`notify-on-submit.gs`](notify-on-submit.gs) and save. Then **Triggers** (the
clock) → **Add Trigger**:

- Function: `sendSupportEmail`
- Event source: **From form**
- Event type: **On form submit**

Save, and allow the permissions Google asks for. Each request then arrives
at your Google account's address with the answers in it, and replying goes
straight to the person who wrote.

Responses also stay in the form's Responses tab, so nothing is lost if an
email goes astray. Google limits a personal account to about 100 such emails
a day.

### 5. Try it

Open the page, choose **I need help**, send a test, and check that the email
arrives. Then choose **I'm reporting a bug** and check that GitHub opens with
every field filled in (no need to post it).
