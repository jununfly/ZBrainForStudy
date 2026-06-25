# Slice #41: Params Schema Validation System

**Node:** 1-4-1-2 (Operations layer type infrastructure)
**Issue:** #41
**Depends On:** #40 (Operation trait and type foundation)

---

## Grill Decisions (Confirmed)

| Q | Answer | Rationale |
|---|--------|-----------|
| Q1: Validation library | **A) `garde` crate** | Lightweight, trait-based, derive support, serde-friendly |
| Q2: Error format | **A) 1:1 match TypeScript format** | Simple string in `suggestion` field; backward-compatible |
| Q3: Integration point | **A) Operation trait `validate()` method** | Rust-idiomatic; dispatch auto-calls validate before handler |

---

## Scope

### 1. Add `garde` dependency to workspace (DONE)
- Add `garde = { version = "0.21", features = ["derive"] }` to workspace Cargo.toml
- Add `garde = { workspace = true }` to `zbrain-core/Cargo.toml`

### 2. Implement 3 Core Validators (TS 1:1 Parity)

#### `validate_upload_path(file_path: &str, root: &str, strict: bool) -> Result<String, OperationError>`
**Exact TS behavior (operations.ts:110-145):**
- Resolve and realpath the file
- ENOENT → `File not found: {path}`
- Other resolve errors → `Cannot resolve path: {path}`
- Always reject final-component symlinks → `Symlinks are not allowed for upload: {path}`
- Lstat race tolerance (pass if realpath succeeded)
- **Strict mode (remote=true):**
  - Realpath root; error if inaccessible → `Confinement root not accessible: {root}`
  - `path.relative(realRoot, realFile)` must NOT be empty, start with `..`, or round-trip
  - Violation → `Upload path must be within the working directory: {path}`
- Returns the canonicalized real path

#### `validate_page_slug(slug: &str) -> Result<(), OperationError>`
**Exact TS behavior (operations.ts:152-165):**
- Empty string → `page_slug must be a non-empty string`
- >255 chars → `page_slug exceeds 255 characters`
- Regex match: `^[a-z0-9{CJK}][a-z0-9{CJK}\-]*(\/[a-z0-9{CJK}][a-z0-9{CJK}\-]*)*$` (case-insensitive)
- Mismatch → `Invalid page_slug: {slug} (allowed: alphanumeric, CJK, hyphens, forward-slash separated segments)`
- CJK ranges: Han (U+4E00-U+9FFF), Hiragana (U+3040-U+309F), Katakana (U+30A0-U+30FF), Hangul Syllables (U+AC00-U+D7AF)

#### `validate_filename(name: &str) -> Result<(), OperationError>`
**Exact TS behavior (operations.ts:197-210):**
- Empty string → `Filename must be a non-empty string`
- >255 chars → `Filename exceeds 255 characters`
- Regex match: `^[a-zA-Z0-9{CJK}][a-zA-Z0-9{CJK}._\-]*$`
- Mismatch → `Invalid filename: {name} (allowed: alphanumeric, CJK, dot, underscore, hyphen — no leading dot/dash, no control chars or backslash)`
- CJK ranges same as page slug

### 3. Add `Validate` Trait to Operation System
```rust
/// Validatable operation params. Mirrors the implicit validation contract
/// in TypeScript where each operation validates its params at handler entry.
pub trait ValidateParams {
    /// Validate params against the schema rules. Returns OperationError with
    /// `invalid_params` code on validation failure.
    fn validate(&self) -> Result<(), OperationError>;
}
```

### 4. Update `Operation` Trait with Validate Hook
```rust
#[async_trait]
pub trait Operation: fmt::Debug + Send + Sync {
    type Params: ValidateParams + serde::de::DeserializeOwned;
    type Output: serde::Serialize;

    // ... existing methods (name, description, local_only) ...

    /// Validate params. Default implementation delegates to `Params::validate()`.
    /// Operations can override for custom validation logic.
    fn validate_params(&self, params: &Self::Params) -> Result<(), OperationError> {
        params.validate()
    }

    /// Execute the operation with validated params.
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> Result<Self::Output, OperationError>;
}
```

### 5. Unit Tests

#### Validator Behavior Parity
- **`validate_upload_path`**:
  - ✅ File not found returns correct error message
  - ✅ Symlink in final path returns correct error
  - ✅ Strict mode: path inside root passes
  - ✅ Strict mode: path outside root (via `..`) returns correct error
  - ✅ Strict mode: parent-dir symlink escape (B5) returns correct error
  - ✅ Loose mode: path outside root passes (local CLI trusted)
  - ✅ Returns canonicalized real path string

- **`validate_page_slug`**:
  - ✅ Empty string rejected
  - ✅ >255 chars rejected
  - ✅ Valid ASCII slug passes: `hello/world-123`
  - ✅ Valid CJK slug passes: `wiki/中文-标题`
  - ✅ Leading slash rejected
  - ✅ Trailing slash rejected
  - ✅ Backslash rejected
  - ✅ URL-encoded traversal rejected

- **`validate_filename`**:
  - ✅ Empty string rejected
  - ✅ >255 chars rejected
  - ✅ Valid ASCII filename passes: `document-v1.0_final.pdf`
  - ✅ Valid CJK filename passes: `会议纪要_2026.docx`
  - ✅ Leading dot rejected (hidden files)
  - ✅ Leading dash rejected (CLI flag confusion)
  - ✅ Backslash rejected
  - ✅ Control chars rejected

#### Error Message Exact Match
- **Critical**: Every error message string must match TS **byte-for-byte**
- Test each validator error case against the exact TS error string

---

## Acceptance Criteria

- [ ] `cargo build -p zbrain-core` succeeds
- [ ] All 3 validators implemented with exact TS behavior parity
- [ ] All error messages match TS byte-for-byte
- [ ] `ValidateParams` trait integrated into `Operation` trait
- [ ] Unit tests cover all validation success + failure cases
- [ ] CJK character support verified for both slug and filename

---

## File Changes

| File | Change |
|------|--------|
| `Cargo.toml` | Add `garde` workspace dependency |
| `crates/zbrain-core/Cargo.toml` | Add `garde = { workspace = true }` |
| `crates/zbrain-core/src/operation.rs` | Add `ValidateParams` trait; add 3 validators; update `Operation` trait with `validate_params` hook |

---

## Estimates

- **Lines of code:** ~250-350 (validators: 150, trait integration: 50, tests: 150)
- **Test cases:** 20+
- **Dependencies:** garde (zero transitive deps)
