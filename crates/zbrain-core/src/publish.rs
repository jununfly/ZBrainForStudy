//! zbrain publish — generate a self-contained, shareable HTML file from a
//! brain markdown page. Deterministic: zero LLM calls.
//!
//! This is a faithful re-implementation of `src/commands/publish.ts`, with one
//! deliberate divergence: markdown is rendered to static HTML **server-side**
//! via `pulldown-cmark` instead of shipping `marked.js` to the browser. For the
//! password-protected path the *rendered HTML* (not the raw markdown) is
//! encrypted, so the in-browser decryptor only has to `innerHTML = decrypted`
//! — there is no client-side markdown renderer anywhere.
//!
//! Encryption is AES-256-GCM with a PBKDF2 (100k rounds, SHA-256) derived key,
//! wire-compatible with the TS `crypto` module so the embedded `DECRYPT_JS`
//! keeps working unchanged.

use std::fmt;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use rand::Rng;
use base64::Engine as _;
use hmac::{Hmac, Mac};
use pulldown_cmark::{html, Parser};
use sha2::Sha256;

/// Characters used for auto-generated passwords (ambiguous ones removed:
/// 0/O, 1/l/I).
const PW_CHARS: &str = "abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";

#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    #[error("decryption failed (wrong password or corrupt data)")]
    Decrypt,
    #[error("base64 decode failed: {0}")]
    Base64(#[from] base64::DecodeError),
}

impl fmt::Display for EncryptedContent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<encrypted salt={} iv={}>", self.salt, self.iv)
    }
}

/// Result of `encrypt_content`. All fields are base64-encoded; `ciphertext`
/// already includes the AES-GCM authentication tag (ciphertext || tag).
#[derive(Debug, Clone)]
pub struct EncryptedContent {
    pub salt: String,
    pub iv: String,
    pub ciphertext: String,
}

// ---------------------------------------------------------------------------
// Content stripping
// ---------------------------------------------------------------------------

/// Strip private/internal data from brain markdown before publishing, mirroring
/// the TS `makeShareable` regex pipeline (order matters).
pub fn make_shareable(content: &str) -> String {
    let mut clean = content.to_string();

    // Frontmatter block.
    clean = replace(r"^---[\s\S]*?---\n*", &clean, "");
    // [Source: ...] citations.
    clean = replace(r"\s*\[Source:[^\]]*\]", &clean, "");
    // **Confirmation:** ABC123
    clean = replace(r"(?i)\*\*Confirmation:\*\*\s*[A-Z0-9]{6,}", &clean, "**Confirmation:** on file");
    // Confirmation: ABC123 / Confirmation# ABC123
    clean = replace(r"(?i)Confirmation[:#]?\s*[A-Z0-9]{6,}", &clean, "Confirmation: on file");
    // conf #ABC123
    clean = replace(r"(?i)\bconf\s*#?\s*[A-Z0-9]{6,}", &clean, "Confirmation: on file");
    // Brain cross-links [Display](./some/path) -> Display (keep display text).
    clean = replace(r"\[([^\]]+)\]\(\.[^)]*\/[^)]+\)", &clean, "$1");
    // "See also:" brain-internal lines (multiline).
    clean = replace(r"(?m)^-?\s*See also:.*$", &clean, "");
    // Timeline section (after the --- separator near the end).
    clean = replace(r"\n---\n\n## Timeline[\s\S]*$", &clean, "");
    // Collapse excessive blank lines.
    clean = replace(r"\n{3,}", &clean, "\n\n");

    clean.trim().to_string()
}

fn replace(re: &str, input: &str, rep: &str) -> String {
    // Unwrap is safe: every pattern above is a valid, fixed regex.
    regex::Regex::new(re).unwrap().replace_all(input, rep).into_owned()
}

/// Extract the H1 title, falling back to "Document".
pub fn extract_title(markdown: &str) -> String {
    let re = regex::Regex::new(r"(?m)^#\s+(.+)$").unwrap();
    match re.captures(markdown) {
        Some(c) => c.get(1).map(|m| m.as_str().trim().to_string()).unwrap_or_else(|| "Document".into()),
        None => "Document".into(),
    }
}

// ---------------------------------------------------------------------------
// Encryption / decryption (AES-256-GCM + PBKDF2)
// ---------------------------------------------------------------------------

const PBKDF2_ITERATIONS: u32 = 100_000;

/// Encrypt plaintext with a password. Wire-compatible with the TS
/// `encryptContent` (salt 16B, iv 12B, PBKDF2 100k/SHA-256 -> 32B key,
/// AES-GCM with appended 16B auth tag).
pub fn encrypt_content(plaintext: &str, password: &str) -> EncryptedContent {
    let salt: [u8; 16] = rand::random();
    let iv: [u8; 12] = rand::random();

    let mut key = [0u8; 32];
    pbkdf2::pbkdf2::<Hmac<Sha256>>(password.as_bytes(), &salt, PBKDF2_ITERATIONS, &mut key)
        .expect("pbkdf2 key derivation");

    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let nonce = Nonce::from_slice(&iv);
    // `encrypt` appends the 16-byte auth tag to the ciphertext.
    let encrypted = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .expect("aes-256-gcm encrypt");

    EncryptedContent {
        salt: BASE64_STANDARD.encode(salt),
        iv: BASE64_STANDARD.encode(iv),
        ciphertext: BASE64_STANDARD.encode(encrypted),
    }
}

/// Inverse of `encrypt_content`. Errors on wrong password or corrupt data.
pub fn decrypt_content(enc: &EncryptedContent, password: &str) -> Result<String, PublishError> {
    let salt = BASE64_STANDARD.decode(&enc.salt)?;
    let iv = BASE64_STANDARD.decode(&enc.iv)?;
    let data = BASE64_STANDARD.decode(&enc.ciphertext)?;

    let mut key = [0u8; 32];
    pbkdf2::pbkdf2::<Hmac<Sha256>>(password.as_bytes(), &salt, PBKDF2_ITERATIONS, &mut key)
        .map_err(|_| PublishError::Decrypt)?;

    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let nonce = Nonce::from_slice(&iv);
    let plaintext = cipher.decrypt(nonce, data.as_slice()).map_err(|_| PublishError::Decrypt)?;
    String::from_utf8(plaintext).map_err(|_| PublishError::Decrypt)
}

/// Generate a memorable-but-unambiguous random password of `length` chars.
pub fn generate_password(length: usize) -> String {
    let chars: Vec<char> = PW_CHARS.chars().collect();
    let mut rng = rand::thread_rng();
    (0..length)
        .map(|_| {
            let b: u8 = rng.gen();
            chars[(b as usize) % chars.len()]
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Markdown rendering + HTML generation
// ---------------------------------------------------------------------------

/// Render markdown to HTML using pulldown-cmark. Raw HTML in the source is
/// passed through by the renderer, so callers should run `sanitize_html`
/// before embedding untrusted content.
pub fn render_markdown(markdown: &str) -> String {
    let parser = Parser::new(markdown);
    let mut out = String::new();
    html::push_html(&mut out, parser);
    out
}

/// Lightweight HTML sanitizer (defense in depth for the static-embed path).
/// Removes the obvious script-injection vectors; the encrypted path relies on
/// the in-browser `sanitizeHtml` for the same purpose.
pub fn sanitize_html(input: &str) -> String {
    let mut s = input.to_string();
    s = replace(r"(?i)<script[\s\S]*?</script>", &s, "");
    s = replace(r"(?i)<iframe[\s\S]*?</iframe>", &s, "");
    s = replace(r"(?i)<object[\s\S]*?</object>", &s, "");
    s = replace(r"(?i)<embed\b[^>]*>", &s, "");
    s = replace(r"(?i)<form[\s\S]*?</form>", &s, "");
    // Strip inline event-handler attributes (onclick=, onload=, ...).
    s = replace(r#"(?i)\s+on\w+\s*=\s*("[^"]*"|'[^']*'|[^\s>]+)"#, &s, "");
    // Neutralize javascript: URIs.
    s = replace(r"(?i)javascript:", &s, "#");
    s
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Generate the final self-contained HTML document.
pub fn generate_html(title: &str, rendered_html: &str, encrypted: Option<&EncryptedContent>) -> String {
    let safe_title = escape_html(title);
    let body = sanitize_html(rendered_html);

    let password_html = if encrypted.is_some() {
        format!(
            r#"
    <div id="pw-overlay" class="pw-overlay">
      <div class="pw-card">
        <div class="pw-lock">&#x1F512;</div>
        <div class="pw-title">{title}</div>
        <div class="pw-subtitle">This document is password protected</div>
        <form id="pw-form">
          <input type="password" id="pw-input" class="pw-input" placeholder="Enter password" autofocus>
          <label class="pw-remember"><input type="checkbox" id="pw-remember" checked> Remember on this device</label>
          <button type="submit" class="pw-btn">Unlock</button>
        </form>
        <div id="pw-error" class="pw-error"></div>
      </div>
    </div>"#,
            title = safe_title
        )
    } else {
        String::new()
    };

    let encrypted_vars = match encrypted {
        Some(e) => format!(
            r#"
    <script>
      window.__SALT = "{salt}";
      window.__IV = "{iv}";
      window.__CT = "{ct}";
    </script>"#,
            salt = e.salt,
            iv = e.iv,
            ct = e.ciphertext
        ),
        None => String::new(),
    };

    let content_script = if encrypted.is_some() {
        format!("<script>{sj}{dj}</script>", sj = SANITIZE_JS, dj = DECRYPT_JS)
    } else {
        String::new()
    };

    let password_css = if encrypted.is_some() { PASSWORD_CSS } else { "" };

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>{title}</title>
<style>{css}{password_css}</style>
</head>
<body>
{password_html}
<div id="content">{body}</div>
{encrypted_vars}
{content_script}
</body>
</html>"#,
        title = safe_title,
        css = CSS,
        password_css = password_css,
        password_html = password_html,
        body = body,
        encrypted_vars = encrypted_vars,
        content_script = content_script
    )
}

// ---------------------------------------------------------------------------
// Embedded assets (ported verbatim from publish.ts; DECRYPT_JS no longer calls
// marked.parse — it injects the decrypted HTML directly).
// ---------------------------------------------------------------------------

const CSS: &str = r#"
  :root {
    --bg: #fafaf9; --fg: #1c1917; --muted: #78716c;
    --accent: #d97706; --border: #e7e5e4; --card-bg: #ffffff;
    --code-bg: #f5f5f4; --link: #2563eb; --error: #dc2626;
  }
  @media (prefers-color-scheme: dark) {
    :root {
      --bg: #0c0a09; --fg: #fafaf9; --muted: #a8a29e;
      --accent: #fbbf24; --border: #292524; --card-bg: #1c1917;
      --code-bg: #1c1917; --link: #60a5fa; --error: #f87171;
    }
  }
  * { box-sizing: border-box; margin: 0; padding: 0; }
  body {
    font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', 'SF Pro', Roboto, sans-serif;
    background: var(--bg); color: var(--fg);
    line-height: 1.7; padding: 1rem;
    max-width: 720px; margin: 0 auto; font-size: 15px;
  }
  h1 { font-size: 1.75rem; font-weight: 700; margin: 1.5rem 0 0.5rem; letter-spacing: -0.02em; }
  h2 { font-size: 1.3rem; font-weight: 600; margin: 2rem 0 0.75rem; padding-bottom: 0.4rem; border-bottom: 2px solid var(--accent); }
  h3 { font-size: 1.1rem; font-weight: 600; margin: 1.5rem 0 0.5rem; color: var(--accent); }
  h4 { font-size: 1rem; font-weight: 600; margin: 1.25rem 0 0.4rem; }
  p { margin: 0.5rem 0; }
  blockquote { border-left: 3px solid var(--accent); padding: 0.75rem 1rem; margin: 1rem 0; background: var(--card-bg); border-radius: 0 8px 8px 0; font-style: italic; color: var(--muted); }
  ul, ol { margin: 0.5rem 0; padding-left: 1.5rem; }
  li { margin: 0.3rem 0; }
  a { color: var(--link); text-decoration: none; }
  a:hover { text-decoration: underline; }
  strong { font-weight: 600; }
  code { background: var(--code-bg); padding: 2px 6px; border-radius: 4px; font-size: 0.9em; }
  hr { border: none; border-top: 1px solid var(--border); margin: 2rem 0; }
  table { width: 100%; border-collapse: collapse; margin: 1rem 0; font-size: 14px; }
  th, td { padding: 8px 12px; border: 1px solid var(--border); text-align: left; }
  th { background: var(--card-bg); font-weight: 600; }
  @media (max-width: 600px) {
    body { font-size: 14px; padding: 0.75rem; }
    h1 { font-size: 1.4rem; }
    h2 { font-size: 1.15rem; }
    table { font-size: 12px; }
    th, td { padding: 6px 8px; }
  }
"#;

const PASSWORD_CSS: &str = r#"
  .pw-overlay {
    position: fixed; inset: 0; display: flex; align-items: center; justify-content: center;
    background: var(--bg); z-index: 1000;
  }
  .pw-card {
    background: var(--card-bg); border: 1px solid var(--border); border-radius: 16px;
    padding: 2.5rem; max-width: 380px; width: 90%; text-align: center;
    box-shadow: 0 4px 24px rgba(0,0,0,0.1);
  }
  .pw-lock { font-size: 3rem; margin-bottom: 1rem; }
  .pw-title { font-size: 1.1rem; font-weight: 600; margin-bottom: 0.5rem; }
  .pw-subtitle { font-size: 0.85rem; color: var(--muted); margin-bottom: 1.5rem; }
  .pw-input {
    width: 100%; padding: 10px 14px; border: 1px solid var(--border); border-radius: 8px;
    background: var(--bg); color: var(--fg); font-size: 15px; margin-bottom: 1rem;
    outline: none; transition: border-color 0.2s;
  }
  .pw-input:focus { border-color: var(--accent); }
  .pw-btn {
    width: 100%; padding: 10px 14px; border: none; border-radius: 8px;
    background: var(--accent); color: #fff; font-size: 15px; font-weight: 600;
    cursor: pointer; transition: opacity 0.2s;
  }
  .pw-btn:hover { opacity: 0.9; }
  .pw-error { color: var(--error); font-size: 0.85rem; margin-top: 0.75rem; display: none; }
  .pw-remember { display: flex; align-items: center; justify-content: center; gap: 6px; margin-bottom: 1rem; font-size: 0.85rem; color: var(--muted); cursor: pointer; }
  .pw-remember input { cursor: pointer; }
  @keyframes shake { 0%,100%{transform:translateX(0)} 25%{transform:translateX(-8px)} 75%{transform:translateX(8px)} }
  .shake { animation: shake 0.3s ease-in-out; }
"#;

const SANITIZE_JS: &str = r#"
    function sanitizeHtml(html) {
      const div = document.createElement('div');
      div.innerHTML = html;
      div.querySelectorAll('script,iframe,object,embed,form').forEach(el => el.remove());
      div.querySelectorAll('*').forEach(el => {
        for (const attr of [...el.attributes]) {
          if (attr.name.startsWith('on') || attr.value.startsWith('javascript:')) {
            el.removeAttribute(attr.name);
          }
        }
      });
      return div.innerHTML;
    }
"#;

const DECRYPT_JS: &str = r#"
const STORAGE_KEY = 'bp_' + location.pathname;

async function deriveKey(password, salt) {
  const enc = new TextEncoder();
  const keyMaterial = await crypto.subtle.importKey('raw', enc.encode(password), 'PBKDF2', false, ['deriveKey']);
  return crypto.subtle.deriveKey(
    { name: 'PBKDF2', salt, iterations: 100000, hash: 'SHA-256' },
    keyMaterial,
    { name: 'AES-GCM', length: 256 },
    false,
    ['decrypt']
  );
}

async function decryptContent(password) {
  try {
    const salt = Uint8Array.from(atob(window.__SALT), c => c.charCodeAt(0));
    const iv = Uint8Array.from(atob(window.__IV), c => c.charCodeAt(0));
    const data = Uint8Array.from(atob(window.__CT), c => c.charCodeAt(0));
    const ciphertext = data.slice(0, data.length - 16);
    const authTag = data.slice(data.length - 16);
    const combined = new Uint8Array(ciphertext.length + authTag.length);
    combined.set(ciphertext);
    combined.set(authTag, ciphertext.length);
    const key = await deriveKey(password, salt);
    const decrypted = await crypto.subtle.decrypt({ name: 'AES-GCM', iv }, key, combined);
    return new TextDecoder().decode(decrypted);
  } catch {
    return null;
  }
}

async function unlock(pw, remember) {
  const result = await decryptContent(pw);
  if (result) {
    if (remember) {
      try { localStorage.setItem(STORAGE_KEY, pw); } catch {}
    }
    document.getElementById('pw-overlay').remove();
    document.getElementById('content').innerHTML = sanitizeHtml(result);
    return true;
  }
  return false;
}

(async () => {
  try {
    const saved = localStorage.getItem(STORAGE_KEY);
    if (saved && await unlock(saved, false)) return;
  } catch {}

  document.getElementById('pw-form').addEventListener('submit', async (e) => {
    e.preventDefault();
    const input = document.getElementById('pw-input');
    const error = document.getElementById('pw-error');
    const card = document.querySelector('.pw-card');
    const remember = document.getElementById('pw-remember').checked;
    const pw = input.value;

    if (await unlock(pw, remember)) return;

    error.style.display = 'block';
    error.textContent = 'Wrong password. Try again.';
    card.classList.remove('shake');
    void card.offsetWidth;
    card.classList.add('shake');
    input.value = '';
    input.focus();
  });

  document.getElementById('pw-input').addEventListener('input', () => {
    document.getElementById('pw-error').style.display = 'none';
  });
})();
"#;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_frontmatter() {
        let md = "---\ntitle: X\n---\n# Hello\nbody";
        assert!(!make_shareable(md).contains("title: X"));
        assert!(make_shareable(md).contains("Hello"));
        assert!(make_shareable(md).contains("body"));
    }

    #[test]
    fn strips_source_citations() {
        // `\s*\[Source:...\]` strips leading whitespace too, but JS and Rust
        // both leave the space between the words -> "See more" (parity-checked).
        let md = "See [Source: https://x.com/1] more";
        assert_eq!(make_shareable(md), "See more");
    }

    #[test]
    fn strips_confirmation_numbers_all_forms() {
        let md = "**Confirmation:** ABC123 and Confirmation# XYZ789 and conf #QQQ111 end";
        let out = make_shareable(md);
        // The 3rd `\bconf...` regex over-matches the word "Confirmation" inside
        // the replacement text (a pre-existing TS quirk), so the specific
        // "**Confirmation:** on file" string does NOT survive. The meaningful
        // guarantees are: confirmation numbers gone, "on file" present.
        assert!(out.contains("on file"));
        assert!(!out.contains("ABC123"));
        assert!(!out.contains("XYZ789"));
        assert!(!out.contains("QQQ111"));
    }

    #[test]
    fn keeps_cross_link_display_text() {
        let md = "Link to [Display Text](./notes/foo) here";
        assert_eq!(make_shareable(md), "Link to Display Text here");
    }

    #[test]
    fn strips_see_also_multiline() {
        let md = "line one\nSee also: something internal\nline three";
        let out = make_shareable(md);
        assert!(!out.contains("See also:"));
        assert!(out.contains("line one"));
        assert!(out.contains("line three"));
    }

    #[test]
    fn strips_timeline_section() {
        let md = "body text\n\n---\n\n## Timeline\n- 2020 did x\n- 2021 did y";
        let out = make_shareable(md);
        assert!(!out.contains("Timeline"));
        assert!(out.contains("body text"));
    }

    #[test]
    fn collapses_blank_lines_and_trims() {
        let md = "\n\n# T\n\n\n\nbody\n\n";
        let out = make_shareable(md);
        assert!(out.starts_with('#'));
        assert!(out.contains(" T"));
        assert!(out.ends_with("body"));
        assert!(!out.contains("\n\n\n"));
    }

    #[test]
    fn extract_title_uses_h1() {
        let mut md = String::new();
        md.push('#');
        md.push_str(" My Title\nbody");
        assert_eq!(extract_title(&md), "My Title");
    }

    #[test]
    fn extract_title_defaults() {
        assert_eq!(extract_title("no heading here"), "Document");
    }

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let enc = encrypt_content("hello world", "s3cret");
        assert_eq!(decrypt_content(&enc, "s3cret").unwrap(), "hello world");
    }

    #[test]
    fn decrypt_wrong_password_fails() {
        let enc = encrypt_content("hello world", "s3cret");
        assert!(decrypt_content(&enc, "wrong").is_err());
    }

    #[test]
    fn decrypt_corrupt_fails() {
        let mut enc = encrypt_content("hello world", "s3cret");
        enc.ciphertext = BASE64_STANDARD.encode("not-valid-ciphertext!!");
        assert!(decrypt_content(&enc, "s3cret").is_err());
    }

    #[test]
    fn generated_password_is_correct_length_and_charset() {
        let pw = generate_password(20);
        assert_eq!(pw.len(), 20);
        assert!(pw.chars().all(|c| PW_CHARS.contains(c)));
        // Ambiguous chars must never appear.
        assert!(!pw.chars().any(|c| c == '0' || c == 'O' || c == '1' || c == 'l' || c == 'I'));
    }

    #[test]
    fn render_markdown_basic() {
        let mut md = String::new();
        md.push('#');
        md.push_str(" Hello\n\nSome **bold** text.");
        let html = render_markdown(&md);
        assert!(html.contains("<h1>Hello</h1>"));
        assert!(html.contains("<strong>bold</strong>"));
    }

    #[test]
    fn generate_html_plain_has_no_marked_or_vars() {
        let html = generate_html("My Page", "<h1>Hi</h1>", None);
        assert!(html.contains("<title>My Page</title>"));
        assert!(html.contains("<h1>Hi</h1>"));
        assert!(!html.contains("marked"));
        assert!(!html.contains("__SALT"));
        assert!(!html.contains("pw-overlay"));
    }

    #[test]
    fn generate_html_encrypted_has_vars_and_overlay() {
        let enc = encrypt_content("<h1>Secret</h1>", "pw");
        let html = generate_html("Secret Page", "<h1>Secret</h1>", Some(&enc));
        assert!(html.contains("__SALT"));
        assert!(html.contains("__IV"));
        assert!(html.contains("__CT"));
        assert!(html.contains("pw-overlay"));
        // No marked.js renderer anywhere.
        assert!(!html.contains("marked.parse"));
    }

    #[test]
    fn xss_in_markdown_is_sanitized_in_static_embed() {
        let rendered = render_markdown("text\n\n<script>alert(1)</script>");
        let html = generate_html("X", &rendered, None);
        assert!(!html.contains("<script>alert(1)</script>"));
    }
}
