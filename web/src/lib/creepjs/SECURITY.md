# Security Policy

## 🛡️ Trusted & Authorized Deployments

The **only** authorized environments for this software are the official source repository and its associated GitHub Pages deployment. The maintainer guarantees the integrity of the code and the privacy of the execution environment **only** when accessed via:

| Platform | URL | Status |
| :--- | :--- | :--- |
| **Source Code** | `https://github.com/abrahamjuliot/creepjs` | ✅ **Official** |
| **Live Demo** | `https://abrahamjuliot.github.io/creepjs` | ✅ **Official** |
| **Localhost** | `localhost` / `127.0.0.1` | ✅ **Safe** |

---

## ⚠️ Rogue Deployments & Phishing Risks

**Any public deployment of this codebase on a custom top-level domain (e.g., `.org`, `.com`, `.net`) is considered an unauthorized and hostile clone.**

### The Security Threat

CreepJS is designed to analyze sensitive browser entropy and fingerprinting vectors. Third-party websites hosting this code are frequently:

1. **Phishing / Social Engineering:** They mimic the official project identity to trick users into trusting the site.
2. **Honeypots:** They may be modified to log, store, and exfiltrate the very fingerprint data they claim to display.
3. **Outdated/Vulnerable:** They often run unpatched versions of the code.

### Policy on External Mirrors

The maintainer **strictly disavows** all external hosted versions.

* **Do not** enter real data or trust the output of CreepJS on any custom domain.
* **Assume** that any external site is actively logging your IP address and device fingerprint.

---

## 🚨 Reporting Impersonators

If you encounter a website claiming to be "CreepJS" or an "Official 2.0" version hosted outside of the `github.io` domain:

1. **DO NOT** trust the site.
2. **Report** the URL to [Google Safe Browsing](https://safebrowsing.google.com/safebrowsing/report_phish/) as a **Social Engineering (Phishing)** site.
3. **Open an Issue** in this repository with the label `security` to alert the community.

---

## 🐛 Reporting Vulnerabilities (Code)

If you discover a security vulnerability within the **actual source code** (not a third-party website), please open a draft security advisory or contact the maintainer directly.

*Note: Reports regarding the behavior of third-party clone sites will be treated as external phishing threats, not code vulnerabilities.*
