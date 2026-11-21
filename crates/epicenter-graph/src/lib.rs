pub mod coverage;
pub mod diff;
pub mod extract;
pub mod graph;

pub use coverage::{CoverageError, CoverageMap, FileCoverage};
pub use diff::{normalize_rel_path, parse_unified_diff, DiffHunk, FileDiff};
pub use extract::{extract_file_symbols, FileSymbols, SourceLanguage, SymbolDef};
pub use graph::{compute_deterministic_blast_score, BlastAnalysisConfig, GraphError, RepoCallGraph};

#[cfg(test)]
mod tests {
    use super::*;
    use epicenter_common::ImpactRing;
    use std::collections::BTreeMap;

    #[test]
    fn parses_multi_file_unified_diff() {
        let diff = r#"diff --git a/src/pricing.py b/src/pricing.py
index 1111111..2222222 100644
--- a/src/pricing.py
+++ b/src/pricing.py
@@ -1,4 +1,5 @@ def compute_total(items):
 def compute_total(items):
-    return sum(i.price for i in items)
+    subtotal = sum(i.price for i in items)
+    return round(subtotal, 2)
diff --git a/src/utils.ts b/src/utils.ts
new file mode 100644
--- /dev/null
+++ b/src/utils.ts
@@ -0,0 +1,3 @@
+export function clamp(x: number): number {
+  return Math.max(0, x);
+}
"#;
        let files = parse_unified_diff(diff);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "src/pricing.py");
        assert_eq!(files[0].hunks.len(), 1);
        assert_eq!(files[0].hunks[0].added_lines, vec![2, 3]);
        assert_eq!(files[0].hunks[0].deleted_lines, vec![2]);
        assert_eq!(
            files[0].hunks[0].context_header.as_deref(),
            Some("def compute_total(items):")
        );
        assert!(files[1].is_new_file);
        assert_eq!(files[1].path, "src/utils.ts");
        assert_eq!(files[1].hunks[0].added_lines, vec![1, 2, 3]);
    }

    #[test]
    fn python_multi_file_blast_radius_and_coverage_dark_zone() {
        let mut sources = BTreeMap::new();
        sources.insert(
            "app/tax.py".to_string(),
            r#"def compute_tax(amount):
    rate = 0.08875
    return amount * rate
"#
            .to_string(),
        );
        sources.insert(
            "app/billing.py".to_string(),
            r#"from app.tax import compute_tax

def charge_invoice(amount):
    tax = compute_tax(amount)
    return amount + tax

def refund_invoice(amount):
    tax = compute_tax(amount)
    return -(amount + tax)
"#
            .to_string(),
        );
        sources.insert(
            "app/routes.py".to_string(),
            r#"from app.billing import charge_invoice, refund_invoice

def post_checkout(req):
    return charge_invoice(req["amount"])

def post_refund(req):
    return refund_invoice(req["amount"])
"#
            .to_string(),
        );

        let diff = r#"diff --git a/app/tax.py b/app/tax.py
--- a/app/tax.py
+++ b/app/tax.py
@@ -1,3 +1,3 @@
 def compute_tax(amount):
-    rate = 0.08875
+    rate = 0.09
     return amount * rate
"#;

        let coverage_json = r#"{
          "files": {
            "app/tax.py": {
              "executed_lines": [1, 2, 3],
              "missing_lines": []
            },
            "app/billing.py": {
              "executed_lines": [3, 4, 5],
              "missing_lines": [7, 8, 9]
            },
            "app/routes.py": {
              "executed_lines": [3, 4],
              "missing_lines": [6, 7]
            }
          }
        }"#;

        let cov = CoverageMap::from_coverage_json(coverage_json).unwrap();
        let graph = RepoCallGraph::from_sources(&sources);
        let report = graph.analyze_diff(diff, Some(&cov), &BlastAnalysisConfig::default());

        assert_eq!(report.ring0_mutated.len(), 1);
        assert_eq!(report.ring0_mutated[0].symbol, "compute_tax");
        assert_eq!(report.ring0_mutated[0].radial_x, 0.0);
        assert_eq!(report.ring0_mutated[0].radial_y, 0.0);
        assert_eq!(report.ring0_mutated[0].test_coverage_pct, Some(100.0));
        assert!(!report.ring0_mutated[0].is_untested_dark_zone);

        assert_eq!(report.ring1_direct_callers.len(), 2);
        let charge = report
            .ring1_direct_callers
            .iter()
            .find(|n| n.symbol == "charge_invoice")
            .expect("charge_invoice in ring1");
        assert_eq!(charge.ring, ImpactRing::Ring1);
        assert_eq!(charge.test_coverage_pct, Some(100.0));
        assert!(!charge.is_untested_dark_zone);
        assert!((charge.radial_x.hypot(charge.radial_y) - 160.0).abs() < 0.5);

        let refund = report
            .ring1_direct_callers
            .iter()
            .find(|n| n.symbol == "refund_invoice")
            .expect("refund_invoice in ring1");
        assert_eq!(refund.test_coverage_pct, Some(0.0));
        assert!(refund.is_untested_dark_zone);

        assert_eq!(report.ring2_transitive_entrypoints.len(), 2);
        let post_refund = report
            .ring2_transitive_entrypoints
            .iter()
            .find(|n| n.symbol == "post_refund")
            .expect("post_refund in ring2");
        assert_eq!(post_refund.ring, ImpactRing::Ring2);
        assert!(post_refund.is_untested_dark_zone);
        assert!((post_refund.radial_x.hypot(post_refund.radial_y) - 280.0).abs() < 0.5);

        assert_eq!(report.dark_zone_count(), 2);
        assert!(report.edges.len() >= 4);
        assert!(report.composite_risk_score > 35.0);
    }

    #[test]
    fn typescript_multi_file_blast_radius_and_lcov_overlay() {
        let mut sources = BTreeMap::new();
        sources.insert(
            "src/cache.ts".to_string(),
            r#"export function buildCacheKey(tenantId: string, key: string): string {
  return `${tenantId}:${key}`;
}
"#
            .to_string(),
        );
        sources.insert(
            "src/sessionStore.ts".to_string(),
            r#"import { buildCacheKey } from "./cache";

export class SessionStore {
  getSession(tenantId: string, token: string) {
    const key = buildCacheKey(tenantId, token);
    return key;
  }
}
"#
            .to_string(),
        );
        sources.insert(
            "src/handler.ts".to_string(),
            r#"import { SessionStore } from "./sessionStore";

export const handleAuth = (store: SessionStore, tenant: string, token: string) => {
  return store.getSession(tenant, token);
};
"#
            .to_string(),
        );

        let diff = r#"diff --git a/src/cache.ts b/src/cache.ts
--- a/src/cache.ts
+++ b/src/cache.ts
@@ -1,3 +1,3 @@
 export function buildCacheKey(tenantId: string, key: string): string {
-  return `${tenantId}:${key}`;
+  return `${tenantId}::${key}`;
 }
"#;

        let lcov = r#"TN:
SF:/workspace/src/cache.ts
DA:1,10
DA:2,10
LF:2
LH:2
end_of_record
SF:/workspace/src/sessionStore.ts
DA:4,0
DA:5,0
DA:6,0
LF:3
LH:0
end_of_record
SF:/workspace/src/handler.ts
DA:3,5
DA:4,5
LF:2
LH:2
end_of_record
"#;

        let cov = CoverageMap::from_lcov(lcov);
        let graph = RepoCallGraph::from_sources(&sources);
        let report = graph.analyze_diff(diff, Some(&cov), &BlastAnalysisConfig::default());

        assert_eq!(report.ring0_mutated.len(), 1);
        assert_eq!(report.ring0_mutated[0].symbol, "buildCacheKey");

        assert!(report
            .ring1_direct_callers
            .iter()
            .any(|n| n.symbol == "SessionStore.getSession" && n.is_untested_dark_zone));
        assert!(report
            .ring2_transitive_entrypoints
            .iter()
            .any(|n| n.symbol == "handleAuth" && !n.is_untested_dark_zone));
    }

    #[test]
    fn fallback_extractor_handles_rust_and_go_files() {
        let mut sources = BTreeMap::new();
        sources.insert(
            "pkg/math.go".to_string(),
            r#"package pkg

func Scale(x int) int {
    return x * 2
}
"#
            .to_string(),
        );
        sources.insert(
            "pkg/api.go".to_string(),
            r#"package pkg

func HandleRequest(x int) int {
    return Scale(x) + 1
}
"#
            .to_string(),
        );
        let diff = r#"diff --git a/pkg/math.go b/pkg/math.go
--- a/pkg/math.go
+++ b/pkg/math.go
@@ -3,3 +3,3 @@
 func Scale(x int) int {
-    return x * 2
+    return x * 3
 }
"#;
        let graph = RepoCallGraph::from_sources(&sources);
        let report = graph.analyze_diff(diff, None, &BlastAnalysisConfig::default());
        assert_eq!(report.ring0_mutated.len(), 1);
        assert_eq!(report.ring0_mutated[0].symbol, "Scale");
        assert_eq!(report.ring1_direct_callers.len(), 1);
        assert_eq!(report.ring1_direct_callers[0].symbol, "HandleRequest");
    }

    #[test]
    fn from_directory_scans_temp_repo_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("core.py"), "def leaf():\n    return 1\n").unwrap();
        std::fs::write(
            src.join("caller.py"),
            "from src.core import leaf\ndef entry():\n    return leaf()\n",
        )
        .unwrap();

        let graph = RepoCallGraph::from_directory(dir.path()).unwrap();
        let diff = "diff --git a/src/core.py b/src/core.py\n--- a/src/core.py\n+++ b/src/core.py\n@@ -1,2 +1,2 @@\n def leaf():\n-    return 1\n+    return 2\n";
        let report = graph.analyze_diff(diff, None, &BlastAnalysisConfig::default());
        assert_eq!(report.ring0_mutated[0].symbol, "leaf");
        assert_eq!(report.ring1_direct_callers[0].symbol, "entry");
    }
}
