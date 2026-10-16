// The support form. A static page cannot post an issue or send mail itself,
// so each choice hands the visitor's words to the place that can: a bug
// report to GitHub's issue form, a request for help to a Google form that
// emails the developer. Both open filled in; the visitor confirms there.
(() => {
  "use strict";

  const config = () => window.SPARKAMP_SUPPORT || {};
  const repo = () => config().repo || "jrssae/sparkamp";

  const form = document.getElementById("support");
  const panels = {
    bug: document.getElementById("bug-fields"),
    help: document.getElementById("help-fields"),
  };
  const button = document.getElementById("continue");
  const pickFirst = document.getElementById("pick-first");
  const status = document.getElementById("status");

  // Fields of .github/ISSUE_TEMPLATE/bug_report.yml, by id. The page's field
  // names match the ids, so a value lands in the box of the same name.
  const ISSUE_FIELDS = ["what-happened", "steps", "expected", "version", "source", "interface", "system", "extra"];
  // The longer fields, dropped from a link that would be too long.
  const LONG_FIELDS = ["what-happened", "steps", "expected", "extra"];
  // Browsers and servers cope with far longer links, but a report that big
  // is better pasted than squeezed into one.
  const MAX_LINK = 7000;

  function show(kind) {
    for (const [name, panel] of Object.entries(panels)) {
      const on = name === kind;
      panel.hidden = !on;
      // A disabled fieldset's fields are left out of validation, so the
      // hidden form's required fields never block the visible one.
      panel.disabled = !on;
    }
    button.hidden = false;
    pickFirst.hidden = true;
    button.textContent = kind === "bug" ? "Continue to GitHub" : "Continue to the Google form";
    say("");
  }

  form.addEventListener("change", (event) => {
    if (event.target.name === "kind") show(event.target.value);
  });

  form.addEventListener("submit", (event) => {
    event.preventDefault();
    if (!form.reportValidity()) return;
    const data = new FormData(form);
    const target = data.get("kind") === "bug" ? bugReport(data) : helpRequest(data);
    if (!target) return;
    // The copy starts before the new tab takes focus, which the clipboard
    // needs; the tab opens here, in the click itself, because Safari blocks
    // one opened later.
    const copying = target.tooLong ? copy(target.tooLong) : null;
    window.open(target.url, "_blank", "noopener");
    if (!copying) {
      say(target.sent, target.url);
      return;
    }
    copying.then((copied) => say(copied ? target.copied : target.notCopied, target.url));
  });

  /** GitHub's new-issue page for the bug-report form, every field filled in. */
  function bugReport(data) {
    const value = (name) => String(data.get(name) || "").trim();
    const url = new URL(`https://github.com/${repo()}/issues/new`);
    url.searchParams.set("template", "bug_report.yml");
    url.searchParams.set("title", value("title"));
    for (const id of ISSUE_FIELDS) {
      if (value(id)) url.searchParams.set(id, value(id));
    }
    const sent = "GitHub opened in a new tab with your report filled in. Press Create there to post it.";
    if (url.href.length <= MAX_LINK) return { url: url.href, sent };
    // Too long for a link: the short fields still arrive filled in, and the
    // whole report goes on the clipboard to paste.
    for (const id of LONG_FIELDS) url.searchParams.delete(id);
    url.searchParams.set("what-happened", "(Paste the full report here.)");
    return {
      url: url.href,
      tooLong: ISSUE_FIELDS.filter((id) => value(id))
        .map((id) => `${labelFor(id)}\n${value(id)}`)
        .join("\n\n"),
      copied: "Your report was too long to send in a link, so it is on your clipboard. Paste it into the first box on GitHub, then press Create.",
      notCopied: "Your report was too long to send in a link. Copy it from this page into the first box on GitHub, then press Create.",
    };
  }

  /** The help form with the visitor's name, email and message filled in. */
  function helpRequest(data) {
    const prefill = config().helpFormPrefill;
    if (!prefill) {
      say("The help form isn't connected yet. Until it is, please ask on GitHub.",
        `https://github.com/${repo()}/issues`, "Open Sparkamp's GitHub issues");
      return null;
    }
    const answers = {
      NAME: String(data.get("name") || "").trim(),
      EMAIL: String(data.get("email") || "").trim(),
      MESSAGE: String(data.get("message") || "").trim(),
    };
    const fill = (message) => {
      const url = new URL(prefill);
      for (const [key, placeholder] of [...url.searchParams]) {
        if (placeholder in answers) url.searchParams.set(key, placeholder === "MESSAGE" ? message : answers[placeholder]);
      }
      return url.href;
    };
    const url = fill(answers.MESSAGE);
    const sent = "The Google form opened in a new tab with your message filled in. Press Submit there to send it.";
    if (url.length <= MAX_LINK) return { url, sent };
    return {
      url: fill("(Paste your message here.)"),
      tooLong: answers.MESSAGE,
      copied: "Your message was too long to send in a link, so it is on your clipboard. Paste it into the message box on the form, then press Submit.",
      notCopied: "Your message was too long to send in a link. Copy it from this page into the message box on the form, then press Submit.",
    };
  }

  function labelFor(id) {
    const field = form.elements[id];
    const label = field && field.labels && field.labels[0];
    return label ? label.firstChild.textContent.trim() : id;
  }

  async function copy(text) {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      return false;
    }
  }

  /** Report what happened, with a link in case the new tab was blocked. */
  function say(message, url, linkText = "Nothing opened? Use this link.") {
    status.replaceChildren();
    if (!message) return;
    const p = document.createElement("p");
    p.textContent = message;
    status.append(p);
    if (url) {
      const a = document.createElement("a");
      a.href = url;
      a.target = "_blank";
      a.rel = "noopener";
      a.textContent = linkText;
      status.append(a);
    }
  }
})();
