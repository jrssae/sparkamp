/**
 * Emails each new Sparkamp help request to the form's owner, with the
 * answers in the message. Google's own notification only says that a
 * response arrived.
 *
 * Runs in the help form's Apps Script project, on an "On form submit"
 * trigger. Setup: docs/support/README.md.
 */
function sendSupportEmail(e) {
  const lines = [];
  let name = "";
  let replyTo = "";
  for (const answer of e.response.getItemResponses()) {
    const title = answer.getItem().getTitle();
    const value = String(answer.getResponse() || "").trim();
    lines.push(title + ":\n" + (value || "(left blank)"));
    if (/email/i.test(title) && value.includes("@")) replyTo = value;
    if (/name/i.test(title)) name = value;
  }

  const message = {
    to: Session.getEffectiveUser().getEmail(),
    subject: "Sparkamp support: " + (name || replyTo || "new request"),
    body:
      lines.join("\n\n") +
      "\n\nSent " + e.response.getTimestamp() + " from the Sparkamp support page.",
  };
  // Reply in your mail app and the answer goes straight to the person.
  if (replyTo) message.replyTo = replyTo;
  MailApp.sendEmail(message);
}
