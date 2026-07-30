use std::path::{Component, Path, PathBuf};

/// Resolve a caller-supplied path against a node's root.
///
/// A relative path joins the root; a path with a leading `/` (or a Windows prefix) escapes it and
/// addresses the host filesystem directly. `.` collapses and `..` pops, so the result never
/// contains either — the root is a default, not a jail.
pub fn resolve_under(root: &Path, path: &str) -> PathBuf {
    let requested = Path::new(path);
    let mut resolved = if requested.has_root() {
        PathBuf::new()
    } else {
        root.to_path_buf()
    };
    for component in requested.components() {
        match component {
            Component::Prefix(prefix) => resolved.push(prefix.as_os_str()),
            Component::RootDir => resolved.push(Component::RootDir.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !resolved.pop() && !resolved.has_root() {
                    resolved.push("..");
                }
            }
            Component::Normal(part) => resolved.push(part),
        }
    }
    resolved
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(root: &str, path: &str) -> String {
        resolve_under(Path::new(root), path).to_string_lossy().to_string()
    }

    #[test]
    fn relative_paths_join_the_root() {
        assert_eq!(resolve("/srv/data", "notes/hello.txt"), "/srv/data/notes/hello.txt");
        assert_eq!(resolve("/srv/data", ""), "/srv/data");
    }

    #[test]
    fn leading_slash_escapes_the_root() {
        assert_eq!(resolve("/srv/data", "/home/allen/projects"), "/home/allen/projects");
        assert_eq!(resolve("/srv/data", "/"), "/");
    }

    #[test]
    fn cur_dir_components_collapse() {
        assert_eq!(resolve("/srv/data", "./notes/./hello.txt"), "/srv/data/notes/hello.txt");
        assert_eq!(resolve("/srv/data", "."), "/srv/data");
    }

    #[test]
    fn parent_dir_pops_a_component() {
        assert_eq!(resolve("/srv/data", "notes/../hello.txt"), "/srv/data/hello.txt");
        assert_eq!(resolve("/srv/data", ".."), "/srv");
        assert_eq!(resolve("/srv/data", "../sibling/file"), "/srv/sibling/file");
    }

    #[test]
    fn parent_dir_at_the_root_stays_at_the_root() {
        assert_eq!(resolve("/srv/data", "/.."), "/");
        assert_eq!(resolve("/srv/data", "/../../etc/hosts"), "/etc/hosts");
    }

    #[test]
    fn parent_dir_past_a_relative_root_is_kept_literal() {
        assert_eq!(resolve("data", "../../elsewhere"), "../elsewhere");
    }
}
