# The gnudb lookup sent the user's email in cleartext

> Found while writing the App Store privacy label, 2026-09-02, and fixed the
> same day. Kept because the reasoning is worth more than the diff: the label
> is what forced someone to write down what was actually being sent.

## What happens

CDDB's `hello` handshake carries `username+hostname+clientname+version`.
`disc::gnudb::hello_param` builds the first two by splitting the address in
Settings at its last `@`:

```
jane@example.org  →  jane+example.org+Sparkamp+1.3.3
```

That goes on **every lookup**, not only on submissions, in the query string of
a request to:

```
http://gnudb.gnudb.org/~cddb/cddb.cgi
```

Plain HTTP. The address travels in cleartext, in a URL, where every hop can
read it and any proxy may log it.

Unset, it sends `anonymous+localhost` and there is nothing personal in the
request at all. Looking a disc up does not need an address; **submitting a
correction back to gnudb does**, which is the only reason to fill the field in.

## It does not have to be cleartext

gnudb answers on HTTPS. Verified:

```
https://gnudb.gnudb.org/~cddb/cddb.cgi   →  200, certificate verifies
http://gnudb.gnudb.org/~cddb/cddb.cgi    →  200
```

So this is a URL scheme and a feature flag, not a protocol limitation.

## What it costs, and why it was not already done

`Cargo.toml` says why:

> the endpoints are plain `http://`, so the default TLS-free feature set
> suffices; deliberately tiny to keep the Flatpak vendor tree lean (no
> reqwest/openssl pull-in)

That was a reasonable call. It is worth revisiting now only because the label
forced someone to write down what is actually being sent, and "a disc ID" and
"the user's email address" are not the same disclosure.

`minreq`'s options:

| Feature | Pulls in | Notes |
|---|---|---|
| `https-rustls` | `rustls`, `webpki-roots`, `rustls-webpki` | Pure Rust, bundled roots. Biggest tree. |
| `https-rustls-probe` | `rustls`, `rustls-native-certs` | Pure Rust, system roots. No bundled CA set to go stale. |
| `https-native` | `native-tls` | Uses the platform's TLS — Secure Transport on macOS, OpenSSL on Linux. Smallest addition on macOS; adds an OpenSSL dependency on Linux, which is exactly what the Flatpak note wanted to avoid. |

`https-rustls-probe` looks like the least-bad on both platforms: no OpenSSL, no
bundled root set to expire.

## Options

1. **Switch to HTTPS.** One feature flag, `BASE_URL` and `SUBMIT_URL`. Costs
   the dependency tree above, on every platform including the Flatpak.
2. **HTTPS on macOS only.** `https-native` behind a `cfg`, keeping Linux as it
   is. Splits behaviour by platform for a privacy property, which is hard to
   justify to the user whose data it is.
3. **Do nothing, and say so.** The field is empty by default and only matters
   to someone who submits corrections. Declare it on the label and warn in the
   Settings UI where the address is entered.
4. **Stop sending it on lookups.** Send `anonymous+localhost` for `query` and
   `read`, and the real address only for `submit`. **This is the cheapest real
   improvement**: it costs no dependency at all, and it removes the address
   from every request except the one that genuinely needs it.

## Decided, 2026-09-02: option 1

**HTTPS, everywhere, on every request.** Josef's call, and he rejected the
recommendation above with it: withholding the address from lookups is not a
defect worth fixing, because a user who has configured an address has chosen to
identify themselves to gnudb, and lookups are how the entries that make gnudb
useful get attributed. What was wrong was not *that* it was sent but *how*.

`minreq` gains `https-rustls-probe`, which adds `rustls` and
`rustls-native-certs` and no OpenSSL — so the Flatpak tree keeps the property
the original comment was protecting. `BASE_URL` and `SUBMIT_URL` are `https://`.

Verified against the live service: `live_gnudb_inserted_disc` and
`live_gnudb_query_real_disc` both pass over TLS, returning all fifteen track
titles for the disc in the drive.

The App Store privacy label still declares Contact Info → Email Address. TLS
changes who can read it in transit; it does not change that it is sent.
