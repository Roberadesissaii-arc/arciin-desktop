//! Which order the queue has to be drained in, and why.
//!
//! A folder cannot be created on the server before its parent exists. That is
//! the only dependency in the whole model, but it is a real one: get it wrong
//! and the server either rejects the child or implicitly creates the parent,
//! and the parent's own create then comes back `ALREADY_EXISTS`.
//!
//! Idempotency covers that — and stays — but it is a net, not a plan. Relying
//! on it means every initial backup does avoidable work and the logs carry
//! errors that are not errors. Ordering is the fix; idempotency is what
//! catches the cases ordering cannot: a lost response, a retry, a restart
//! mid-operation.
//!
//! # The rule
//!
//! ```txt
//! folders, shallowest first   WebProject
//!                             WebProject/public
//! then files                  WebProject/public/logo.png
//! ```
//!
//! Sorting by depth is what makes "parent before child" true globally rather
//! than per-batch: a parent is always strictly shallower than its child, so
//! depth order puts every ancestor ahead of every descendant without having to
//! build a dependency graph.

/// How deep a relative path sits, counting from the root.
///
/// Scanned paths use forward slashes regardless of platform, so this counts
/// separators rather than consulting `Path`. A root-level entry is depth 0.
pub fn depth(relative_path: &str) -> usize {
    relative_path
        .trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .count()
        .saturating_sub(1)
}

/// Every ancestor folder of a path, shallowest first.
///
/// Written for the watcher that comes next, not just for the initial scan.
/// A watcher sees events in whatever order Windows reports them, so it will
/// routinely learn about a file before the folder holding it; it needs the
/// same answer this gives — "these folders have to exist first, in this
/// order" — without re-deriving it from a sort over the whole queue.
pub fn ancestors(relative_path: &str) -> Vec<String> {
    let segments: Vec<&str> = relative_path
        .trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();

    // The last segment is the entry itself, not an ancestor.
    (1..segments.len())
        .map(|end| segments[..end].join("/"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depth_counts_from_the_root() {
        assert_eq!(depth("photo.jpg"), 0);
        assert_eq!(depth("WebProject"), 0);
        assert_eq!(depth("WebProject/public"), 1);
        assert_eq!(depth("WebProject/public/logo.png"), 2);
        assert_eq!(depth("a/b/c/d/e"), 4);
    }

    #[test]
    fn depth_ignores_stray_separators() {
        assert_eq!(depth("/WebProject/"), 0);
        assert_eq!(depth("WebProject//public"), 1);
        assert_eq!(depth(""), 0);
    }

    #[test]
    fn a_parent_is_always_shallower_than_its_child() {
        // The property the whole ordering rests on.
        for (parent, child) in [
            ("WebProject", "WebProject/public"),
            ("WebProject/public", "WebProject/public/logo.png"),
            ("a", "a/b/c"),
        ] {
            assert!(
                depth(parent) < depth(child),
                "{parent} must sort before {child}"
            );
        }
    }

    #[test]
    fn ancestors_are_listed_parent_first() {
        assert_eq!(
            ancestors("WebProject/public/logo.png"),
            vec!["WebProject".to_string(), "WebProject/public".to_string()]
        );
    }

    #[test]
    fn a_root_level_entry_has_no_ancestors() {
        assert!(ancestors("photo.jpg").is_empty());
        assert!(ancestors("WebProject").is_empty());
        assert!(ancestors("").is_empty());
    }

    #[test]
    fn every_ancestor_is_shallower_than_the_entry() {
        let path = "a/b/c/d/logo.png";
        let entry_depth = depth(path);
        for (index, ancestor) in ancestors(path).iter().enumerate() {
            assert_eq!(depth(ancestor), index);
            assert!(depth(ancestor) < entry_depth);
        }
    }

    #[test]
    fn ancestors_and_depth_agree_on_count() {
        // A path's depth is exactly how many folders have to exist first.
        for path in ["x.txt", "a/x.txt", "a/b/x.txt", "a/b/c/x.txt"] {
            assert_eq!(ancestors(path).len(), depth(path));
        }
    }
}
