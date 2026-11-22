//! Built-in coding workflow slash commands owned by `merry-coding`.
//!
//! These workflows compile into ordinary user-turn prompts so interactive
//! surfaces can trigger structured review flows without mutating the stable
//! system prefix or provider tool catalog (preserving KV cache reuse).

/// Built-in coding workflow command exposed to interactive surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CodingWorkflowCommand {
    /// Multi-angle skeptical code review with 3-state verification.
    CodeReview,
    /// High-confidence security audit with explicit false-positive filtering.
    SecurityReview,
}

impl CodingWorkflowCommand {
    /// Canonical slash command name (without leading `/`).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::CodeReview => "code-review",
            Self::SecurityReview => "security-review",
        }
    }

    /// Secondary slash aliases (without leading `/`).
    #[must_use]
    pub const fn aliases(self) -> &'static [&'static str] {
        match self {
            Self::CodeReview => &["review"],
            Self::SecurityReview => &["sec-review"],
        }
    }

    /// Short description for help and completion menus.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::CodeReview => "Review recent changes or a specified target",
            Self::SecurityReview => "Audit recent changes or a target for security risks",
        }
    }

    /// Human-readable optional argument hint.
    #[must_use]
    pub const fn argument_hint(self) -> &'static str {
        "[scope or focus]"
    }

    /// Renders the user-turn prompt for this workflow command.
    ///
    /// Keeps the stable system prefix and tool definitions unchanged while
    /// structuring multi-angle analysis, skeptical verification, and
    /// context-aware delegation to Merry's built-in `review` subagent.
    #[must_use]
    pub fn build_prompt(self, scope_or_focus: Option<&str>) -> String {
        let scope = scope_or_focus
            .map(str::trim)
            .filter(|value| !value.is_empty());
        match self {
            Self::CodeReview => build_code_review_prompt(scope),
            Self::SecurityReview => build_security_review_prompt(scope),
        }
    }
}

fn build_code_review_prompt(scope: Option<&str>) -> String {
    let phase_zero = match scope {
        Some(target) => format!(
            "## Phase 0 — Gather the scope\n\
Target scope / focus requested by user:\n\
{target}\n\n\
Inspect the requested scope directly (files, commits, diff range, or subsystem) along with its enclosing functions, callers, and invariants."
        ),
        None => "## Phase 0 — Gather the diff\n\
Inspect uncommitted workspace changes first (`git status --short`, `git diff`, `git diff --cached`, and any untracked files). \
If the working tree is clean, inspect the current branch diff against the default branch (`git diff origin/HEAD...HEAD`, falling back to `git diff main...HEAD` or `git diff HEAD~1..HEAD`).".to_owned(),
    };

    format!(
        "Perform a precision-focused, skeptical code review. Every finding you surface must be actionable and worth fixing.\n\n\
{phase_zero}\n\n\
## Delegation rule (context-aware)\n\
- If you authored or edited code in this session, OR the diff spans multiple files / non-trivial logic, AND `spawn_subagents` is available, delegate independent discovery and/or Phase 2 verification to independent `review` subagent(s) (`role: \"review\"`) so they inspect the diff from an unanchored fresh context. Brief them strictly with the scope, diff/files, and known user constraints—never pass optimistic conclusions or self-justifications. Verify key `path:line` citations yourself before presenting the final report.\n\
- If this is already a fresh session reviewing a narrow/single-file target, or subagents are disabled/unavailable, run all phases directly.\n\n\
## Phase 1 — Multi-angle candidate discovery\n\
Inspect the diff from all five angles:\n\
1. **Line-by-line & enclosing-function scan**: Read every hunk line by line and read the enclosing function (unchanged lines re-exposed by the diff are in scope). Ask what input, state, timing, or platform makes each line wrong: inverted conditions, off-by-one, empty/zero/null handling, missing `await`, swallowed errors, copy-paste variable mistakes, unescaped metacharacters.\n\
2. **Removed-behavior audit**: For every line deleted or replaced, name the invariant or guard it enforced and confirm where the new code re-establishes it (dropped guards, lost error paths, narrowed validation, removed test coverage).\n\
3. **Cross-file caller & callee tracing**: Search callers (`rg`) for changed functions/types/contracts and check call sites for broken preconditions, changed return shapes, new errors, or ordering/timing regressions.\n\
4. **Project rules & conventions (`AGENTS.md`)**: Check applicable `AGENTS.md` files (repo root and ancestor directories of changed files) for clear violations of stated project rules, quoting the exact rule and offending line.\n\
5. **Efficiency & hot-path regressions**: Flag concrete wasted work introduced by the diff (redundant I/O or computation in loops/hot paths, blocking work in async paths, avoidable serialization, long-lived closures retaining large scopes) and name the cheaper alternative.\n\n\
## Phase 2 — Skeptical 3-state verification\n\
Deduplicate overlapping candidates, then verify each remaining candidate against exact source lines (and targeted checks/tests when helpful). Classify each candidate as exactly one of:\n\
- **CONFIRMED**: Can name exact inputs/state that trigger the bug and the wrong output or crash; quote the exact line.\n\
- **PLAUSIBLE**: Real mechanism, trigger depends on timing, environment, or configuration; state exactly what would confirm it.\n\
- **REFUTED**: Factually wrong or already guarded elsewhere — drop it and do not report it.\n\
Keep only **CONFIRMED** and **PLAUSIBLE** findings.\n\n\
## Review constraints\n\
- Read-only review: do not modify production files or apply fixes inline unless the user asks afterward.\n\
- Group repeated occurrences of the exact same mistake into one finding and list all affected `path:line` locations.\n\n\
## Output format\n\
1. **Verdict**: `PASS`, `FAIL`, or `BLOCKED` (naming the exact reviewed scope).\n\
2. **Findings** ordered by severity (`Important` -> `Pre-existing` -> `Nit`), or explicitly state no material issues found:\n\
   - `Important`: bug added or re-exposed by this change that should be fixed before merging (a rare trigger does not lower severity).\n\
   - `Pre-existing`: real bug in unchanged code that this change did not introduce.\n\
   - `Nit`: minor non-blocking issue worth fixing (e.g. a broken `AGENTS.md` rule or misleading diagnostic).\n\
   - For each finding include: `[CONFIRMED|PLAUSIBLE]`, exact `path:line`, and 2-3 concise sentences (user-visible problem -> exact location and trigger -> `Fix:` concrete change).\n\
3. **Verification**: exact commands/checks executed and their outcomes (or what was skipped and why)."
    )
}

fn build_security_review_prompt(scope: Option<&str>) -> String {
    let phase_zero = match scope {
        Some(target) => format!(
            "## Target scope\n\
Target scope / focus requested by user:\n\
{target}\n\n\
Audit the requested scope and trace every trust boundary, untrusted input source, and privileged sink it touches."
        ),
        None => "## Target scope\n\
Audit newly introduced security implications in the pending changes: inspect uncommitted changes first (`git status --short`, `git diff`, `git diff --cached`, and untracked files); if the working tree is clean, inspect the branch diff (`git diff origin/HEAD...HEAD`, falling back to `git diff main...HEAD` or `git diff HEAD~1..HEAD`). Focus on security risks introduced or worsened by these changes rather than unrelated legacy code.".to_owned(),
    };

    format!(
        "Perform a high-confidence security review focused strictly on concrete, exploitable vulnerabilities. Better to report zero findings than flood the review with theoretical noise.\n\n\
{phase_zero}\n\n\
## Delegation rule (context-aware)\n\
- Prefer delegating to independent `review` subagent(s) (`role: \"review\"`) whenever `spawn_subagents` is available and either you authored edits in this session OR the audit spans multiple files / trust boundaries: first identify candidate vulnerabilities from an unanchored context, then run a skeptical false-positive verification pass applying the Hard Exclusions and Confidence >= 8 gate below. Verify key `path:line` citations before presenting the final report.\n\
- If this is already a fresh session auditing a narrow single-file target, or subagents are disabled/unavailable, execute Phases 1-3 directly while strictly enforcing the same false-positive filter.\n\n\
## Phase 1 — Repository security context\n\
Before judging the diff, inspect existing security boundaries, validation/sanitization helpers, permission/sandbox policy, and authentication/authorization patterns in the repository.\n\n\
## Phase 2 — Comparative & dataflow vulnerability analysis\n\
Compare the changed code against existing secure patterns and trace untrusted inputs (`Source -> Sink`) across trust boundaries:\n\
- **Input validation & injection**: command/shell injection, SQL/NoSQL injection, template/XXE injection, XSS (reflected/stored/DOM, excluding auto-escaped framework views unless unsafe escape hatches are used), unsafe deserialization / code execution.\n\
- **AuthN / AuthZ & boundary bypasses**: authentication or authorization bypasses, privilege escalation, session/token validation flaws, sandbox/permission bypasses, untrusted project config weakening global trust policy.\n\
- **Filesystem & network safety**: path traversal, symlink/TOCTOU escapes, SSRF where an attacker controls the host or protocol, unsafe temp files.\n\
- **Crypto, secrets & data exposure**: hardcoded credentials/API keys, weak crypto or broken certificate validation, plaintext logging or API leakage of high-value secrets or PII.\n\
- **Memory safety**: only when actual `unsafe` blocks or FFI boundaries violate soundness invariants.\n\n\
## Phase 3 — False-positive filtering & confidence gate\n\
Read the code directly to verify exploitability; do not edit production files. Automatically exclude findings matching any of these **Hard Exclusions**:\n\
1. Denial of Service (DoS), rate-limiting, CPU/memory exhaustion, or resource/FD leaks.\n\
2. Missing defense-in-depth hardening or theoretical best practices without a concrete exploit path.\n\
3. Theoretical race conditions or timing attacks without a concrete, practical trigger.\n\
4. Outdated third-party dependency advisories (handled separately), test-only files, or Markdown documentation.\n\
5. Memory-safety claims in safe Rust or other memory-safe languages without `unsafe`/FFI.\n\
6. Log spoofing or logging non-secret, non-PII data (such as ordinary URLs); missing audit logs.\n\
7. SSRF where the attacker controls only the URL path (not host or protocol); regex injection / ReDoS.\n\
8. Missing permission checks in untrusted client-side code when enforced server-side.\n\
9. Attacks assuming the attacker already controls trusted local CLI flags or host environment variables (unless the project's threat model explicitly treats them as untrusted).\n\n\
Score every surviving finding from 1 to 10 for exploitability confidence:\n\
- **8-10**: Clear, concrete exploit path verified in code -> **REPORT**.\n\
- **Below 8**: Speculative or unverified -> **DROP**.\n\n\
## Output format\n\
1. **Verdict**: `PASS`, `FAIL`, or `BLOCKED` (naming the exact audited scope).\n\
2. **Findings** (High and concrete Medium findings with confidence >= 8/10 only, ordered by severity; if none survive, explicitly state no high-confidence exploitable vulnerabilities were found):\n\
   - Heading: `### Vuln N: <Category>: path:line`\n\
   - `Severity`: `High` (directly exploitable RCE, auth bypass, sandbox escape, or data breach; local-network exploits still count) or `Medium` (concrete impact under specific conditions)\n\
   - `Confidence`: `<8-10>/10`\n\
   - `Description`: exact flaw and broken trust boundary\n\
   - `Exploit Scenario`: concrete attacker model, input/trigger path, and resulting impact\n\
   - `Recommendation`: minimal, concrete fix aligned with repository patterns\n\
3. **Residual risks**: any trust boundaries in scope that could not be verified from the available code."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_command_metadata_and_prompts_are_well_formed() {
        assert_eq!(CodingWorkflowCommand::CodeReview.name(), "code-review");
        assert_eq!(CodingWorkflowCommand::CodeReview.aliases(), &["review"]);
        assert_eq!(
            CodingWorkflowCommand::SecurityReview.name(),
            "security-review"
        );
        assert_eq!(
            CodingWorkflowCommand::SecurityReview.aliases(),
            &["sec-review"]
        );

        let default_code = CodingWorkflowCommand::CodeReview.build_prompt(None);
        assert!(default_code.contains("git diff"));
        assert!(default_code.contains("role: \"review\""));
        assert!(default_code.contains("Removed-behavior audit"));
        assert!(default_code.contains("Cross-file caller & callee tracing"));
        assert!(default_code.contains("AGENTS.md"));
        assert!(default_code.contains("CONFIRMED"));
        assert!(default_code.contains("PLAUSIBLE"));
        assert!(default_code.contains("REFUTED"));
        assert!(default_code.contains("Read-only review"));

        let scoped_code = CodingWorkflowCommand::CodeReview.build_prompt(Some("  main..HEAD  "));
        assert!(scoped_code.contains("main..HEAD"));
        assert!(!scoped_code.contains("If the working tree is clean"));

        let blank_security = CodingWorkflowCommand::SecurityReview.build_prompt(Some("   "));
        assert!(blank_security.contains("git diff"));
        assert!(blank_security.contains("Hard Exclusions"));
        assert!(blank_security.contains("Confidence >= 8"));
        assert!(blank_security.contains("Exploit Scenario"));
        assert!(blank_security.contains("path traversal"));

        let scoped_security = CodingWorkflowCommand::SecurityReview
            .build_prompt(Some("crates/merry-tools/src/ssrf.rs"));
        assert!(scoped_security.contains("crates/merry-tools/src/ssrf.rs"));
        assert!(scoped_security.contains("role: \"review\""));
    }
}
