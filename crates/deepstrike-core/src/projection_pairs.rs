//! F5 projection-pair registry + alias-discipline gate (0.2.66, whole file `#[cfg(test)]`).
//!
//! P8 §1 改判：pre-ABI 与 wire 的同名双生类型族**不是双权威**——每族是一处权威
//! （wire/ABI 版）+ 一份更丰富的内部语义词表 + 边界穷尽投影。本模块是登记表与机器门禁：
//!
//! 1. **登记表**（`PROJECTION_PAIRS`）：每族两侧位置 + 转换缝。改词表先改这里。
//! 2. **别名纪律**：wire 版是 ABI 权威。wire 模块之外的任何 `use` 引入 wire 双生类型
//!    必须带权威方向别名（`ToolCall as WireToolCall`，先例 driver.rs:100）。
//!    门禁测试扫描全 core 源码：裸 import 即红——违例应当是 CI 红，不是复盘发现。
//! 3. 新成员规则不变：内部词表加变体 → 编译器经穷尽 match 强制更新转换缝；
//!    ABI 词表加变体 → ABI rev 流程。
//!
//! 特注：`ParamConstraint`（governance/constraint.rs）无 wire 双生，单族，不登记；
//! `EntropyWatchConfig`（scheduler/entropy.rs）↔ `EntropyWatchPolicy`（wire/config.rs）
//! 是**异名** projection 对（转换缝在 syscall/config 投影），同名扫描天然覆盖不到，登记于此。

// One tree per `use` statement is the codebase convention; the span parser below relies on it.
#[cfg(test)]
mod tests {
    /// (family, internal site, wire site, conversion seam) — the registration table.
    const PROJECTION_PAIRS: &[(&str, &str, &str, &str)] = &[
        (
            "TerminationReason",
            "types/result.rs",
            "wire/terminal.rs",
            "driver agent_terminal / termination_from_label",
        ),
        (
            "PaceAction+PaceDecision",
            "types/result.rs",
            "wire/terminal.rs",
            "driver agent_terminal (exhaustive inline match)",
        ),
        (
            "ToolCall+ToolResult+ToolSchema",
            "types/message.rs",
            "wire/effect.rs",
            "driver core_tool_call / wire_tool_call",
        ),
        (
            "ResourceQuota",
            "governance/quota.rs",
            "wire/config.rs",
            "driver wire→governance projection",
        ),
        (
            "KnowledgeEntry",
            "context/partitions.rs",
            "wire/root.rs",
            "driver root projection",
        ),
        (
            "MilestoneCheckResult",
            "types/milestone.rs",
            "wire/effect.rs",
            "driver milestone projection",
        ),
        (
            "VerificationContract",
            "types/contract.rs",
            "wire/config.rs",
            "driver config projection",
        ),
        (
            "WorkflowNode+WorkflowSpec",
            "orchestration/workflow/mod.rs",
            "wire/root.rs",
            "driver root projection",
        ),
        (
            "RenderedContext (F3: internal renamed InternalRenderedContext, 0.2.66)",
            "context/renderer.rs",
            "wire/effect.rs",
            "driver rendered_context",
        ),
    ];

    /// Wire-side type names that carry the alias discipline outside `runtime/kernel/wire/`.
    const ALIAS_GATED_TYPES: &[&str] = &[
        "TerminationReason",
        "PaceAction",
        "PaceDecision",
        "ToolCall",
        "ToolResult",
        "ToolSchema",
        "ResourceQuota",
        "KnowledgeEntry",
        "MilestoneCheckResult",
        "VerificationContract",
        "WorkflowNode",
        "WorkflowSpec",
        "RenderedContext",
    ];

    /// Every `+`-separated member named by the registry must be alias-gated, so a new
    /// family cannot land half-registered.
    #[test]
    fn registry_families_are_fully_alias_gated() {
        for (family, _, _, _) in PROJECTION_PAIRS {
            let head = family.split(" (").next().unwrap();
            for member in head.split('+') {
                let member = member.trim();
                assert!(
                    ALIAS_GATED_TYPES.contains(&member),
                    "family {family} member `{member}` is registered but not alias-gated — \
                     add it to ALIAS_GATED_TYPES"
                );
            }
        }
    }

    /// Strip `/*…*/` block comments then `//…` line comments so prose can never
    /// masquerade as a `use` statement. String literals are not preserved — fine
    /// for a lint-shaped gate that only inspects `use` spans.
    fn strip_comments(src: &str) -> String {
        let mut no_block = String::with_capacity(src.len());
        let mut rest = src;
        while let Some(i) = rest.find("/*") {
            no_block.push_str(&rest[..i]);
            match rest[i + 2..].find("*/") {
                Some(j) => rest = &rest[i + 2 + j + 2..],
                None => return no_block,
            }
        }
        no_block.push_str(rest);
        no_block
            .lines()
            .map(|l| match l.find("//") {
                Some(i) => &l[..i],
                None => l,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The imported items of one `use` span (single-tree convention): the braced list,
    /// or the final path segment when unbraced.
    fn imported_items(span: &str) -> Vec<String> {
        let body = span
            .trim()
            .trim_start_matches("pub")
            .trim_start()
            .trim_start_matches("use")
            .trim();
        match body.find('{') {
            Some(i) => body[i + 1..]
                .trim_end()
                .trim_end_matches('}')
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            None => vec![body.rsplit("::").next().unwrap_or(body).trim().to_string()],
        }
    }

    #[test]
    fn wire_dual_family_imports_outside_the_wire_module_carry_a_direction_alias() {
        let manifest = env!("CARGO_MANIFEST_DIR");
        let src = std::path::Path::new(manifest).join("src");
        let files = walk(&src);
        assert!(
            files.len() > 50,
            "source walk found only {} files — the gate went blind",
            files.len()
        );

        let mut violations = Vec::new();
        for path in &files {
            let rel = path
                .strip_prefix(&src)
                .unwrap()
                .to_string_lossy()
                .to_string();
            if rel.starts_with("runtime/kernel/wire/") {
                continue; // the wire module owns the ABI vocabulary
            }
            let raw = std::fs::read_to_string(path).unwrap();
            let text = strip_comments(&raw);
            let mut rest = text.as_str();
            while let Some(i) = find_use_keyword(rest) {
                let span_end = rest[i..].find(';').map(|e| i + e).unwrap_or(rest.len());
                let span = rest[i..span_end].trim();
                if span.contains("wire") {
                    for item in imported_items(span) {
                        for t in ALIAS_GATED_TYPES {
                            if item == *t {
                                let line =
                                    text[..text.find(span).unwrap_or(0)].matches('\n').count() + 1;
                                violations.push(format!(
                                    "{rel}:{line}: wire type `{t}` imported without a direction \
                                     alias (expected `{t} as Wire{t}` — F5 alias discipline, see \
                                     the projection_pairs registry header)"
                                ));
                            }
                        }
                    }
                }
                if span_end >= rest.len() {
                    break;
                }
                rest = &rest[span_end..];
            }
        }
        assert!(
            violations.is_empty(),
            "F5 alias-discipline violations:\n{}",
            violations.join("\n")
        );
    }

    /// Find the next `use` keyword at a token boundary (start of file, whitespace,
    /// `;` or `{` before it) — never mid-identifier.
    fn find_use_keyword(s: &str) -> Option<usize> {
        let mut from = 0;
        while let Some(i) = s[from..].find("use ") {
            let at = from + i;
            let boundary = at == 0
                || s[..at]
                    .chars()
                    .next_back()
                    .map(|c| c.is_whitespace() || c == ';' || c == '{' || c == '>')
                    .unwrap_or(false);
            if boundary {
                return Some(at);
            }
            from = at + 4;
        }
        None
    }

    fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            let Ok(read) = std::fs::read_dir(&d) else {
                continue;
            };
            for e in read.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().map(|x| x == "rs").unwrap_or(false) {
                    out.push(p);
                }
            }
        }
        out.sort();
        out
    }
}
