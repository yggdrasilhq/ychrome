# ipindiaonline.gov.in

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## tls-jumbled-chain-and-esign-not-dsc · PARTIAL
task: 
model: claude-opus-5
date: 2026-07-31
tags: 

The trademark e-filing portal (Comprehensive e-Filing Services) is reachable ONLY over
curl, and only after a trust-store fix. Both defects below are the site's, not ours.

## 1. The TLS chain is jumbled and kills GnuTLS (browsers/WebKitGTK)

The server sends **7 certificates** containing **two different paths** for the same
intermediate `EM DV TLS CA - G2A-1`:

- a DEAD path issued by Comodo `AAA Certificate Services` — a retired root that Debian
  removed from `ca-certificates` — and it is listed FIRST;
- a GOOD path issued by `emSign Root TLS CA - G1` -> `emSign Root CA - G1`, which Debian
  ships and trusts.

OpenSSL and GnuTLS both match the leaf's issuer against the first candidate in the
presented order, walk into the AAA branch, and fail with a generic error. `curl` says
"failed to verify the legitimacy of the server"; WebKitGTK says `Unacceptable TLS
certificate`. Neither message hints that a perfectly valid path is sitting in the same
handshake.

**Fix for curl/OpenSSL (no new authority granted):** extract the emSign-issued copy of
`EM DV TLS CA - G2A-1` from the handshake and install it as an anchor. It already
verifies against the stock Debian store, is `CA:TRUE, pathlen:0`, and its EKU is limited
to TLS server/client auth, so trusting it adds nothing that was not already implied.

```sh
echo | openssl s_client -connect ipindiaonline.gov.in:443 \
    -servername ipindiaonline.gov.in -showcerts 2>/dev/null > chain.pem
# split, then pick the cert whose subject is "EM DV TLS CA - G2A-1"
# AND whose issuer is "emSign Root TLS CA - G1"  (NOT the AAA-issued twin)
sudo install -m 644 emudhra.pem \
    /usr/local/share/ca-certificates/emudhra-em-dv-tls-ca-g2a-1.crt
sudo update-ca-certificates
```

sha256 of the correct cert:
`3F:13:59:7D:33:55:D3:41:BD:09:32:8A:EE:56:7D:7F:4E:00:2D:99:19:B9:86:F9:96:2D:AF:F1:97:EA:75:D1`

**GnuTLS is NOT fixed by this** and therefore neither is ychrome. Measured: GnuTLS walks
the presented order and will not search for an alternate anchor, so the only thing that
satisfies it is trusting the retired `AAA Certificate Services` root itself. That is a
much broader change — do not do it casually, and prefer curl.

## 2. Drive it in curl

Login page `user/frmloginnew.aspx` is plain ASP.NET ViewState:
`TBUserName`, `TBPassword`, `txtCaptcha`, submit `LnkSubmitLogin`, plus a CAPTCHA image.
Fetch the CAPTCHA over the same cookie jar and read the image directly rather than
reaching for a browser.

## 3. The signing gate is eSign, not a DSC dongle

Every third-party guide on the web says a physical Class-3 DSC token is mandatory for
Form TM-A. **The portal's own FAQ contradicts them** (`Extras/FAQESign.aspx`): eSign is
integrated into the IPO e-filing portal and lets an applicant sign
"without having to obtain a physical digital signature dongle", authenticating by
**Aadhaar e-KYC OTP**. Sole authorised provider today is eMudhra; the portal links an
account-creation URL for it. Certificates live 30 minutes, keys are single-use.

Useful in-portal documents, all readable over curl once the trust fix is in:
`UsefullDownloads/e-usermanual.pdf`, `UsefullDownloads/DSCManual.pdf`,
`UsefullDownloads/eMudhra_eSign_User_Guide.pdf`, `Extras/RegistrationSteps.aspx`,
`user/How-To-Register.aspx`.
