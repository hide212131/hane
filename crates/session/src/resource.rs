use std::path::{Path, PathBuf};

/// Resolves relative resource references (image destinations today, includes
/// later) against a base directory.
///
/// The base is the session's own file directory, not the process working
/// directory, so the same document resolves the same way no matter where the
/// app was launched from or which session is active.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ResourceResolver {
    base: Option<PathBuf>,
}

impl ResourceResolver {
    /// A resolver with no base: relative destinations stay relative and are left
    /// for the caller to reject or render as missing.
    pub fn detached() -> Self {
        Self { base: None }
    }

    pub fn for_directory(directory: Option<&Path>) -> Self {
        Self {
            base: directory.map(Path::to_path_buf),
        }
    }

    pub fn base(&self) -> Option<&Path> {
        self.base.as_deref()
    }

    /// Absolute destinations are returned unchanged; relative ones are joined
    /// onto the base directory.
    pub fn resolve(&self, destination: &str) -> PathBuf {
        let destination = Path::new(destination);
        if destination.is_absolute() {
            return destination.to_path_buf();
        }
        match &self.base {
            Some(base) => base.join(destination),
            None => destination.to_path_buf(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_destinations_resolve_against_the_document_directory() {
        let resolver = ResourceResolver::for_directory(Some(Path::new("/notes/posts")));
        assert_eq!(
            resolver.resolve("assets/feather.svg"),
            PathBuf::from("/notes/posts/assets/feather.svg")
        );
        assert_eq!(
            resolver.resolve("/tmp/feather.svg"),
            PathBuf::from("/tmp/feather.svg")
        );
    }

    #[test]
    fn an_untitled_document_does_not_borrow_the_working_directory() {
        let resolver = ResourceResolver::detached();
        assert_eq!(
            resolver.resolve("assets/feather.svg"),
            PathBuf::from("assets/feather.svg")
        );
        assert_eq!(resolver.base(), None);
    }
}
