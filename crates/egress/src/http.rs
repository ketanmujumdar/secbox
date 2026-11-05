/// A method must be an uppercase token. HTTP methods are case-sensitive, so
/// `get` is not `GET`, and passing it to a lenient server would skip the rule.
pub(crate) fn is_valid_method(method: &str) -> bool {
    !method.is_empty() && method.len() <= 32 && method.bytes().all(|b| b.is_ascii_uppercase())
}

/// A request target must be origin-form: `/` followed by visible ASCII.
pub(crate) fn is_valid_path(path: &str) -> bool {
    path.starts_with('/') && path.bytes().all(|b| b.is_ascii_graphic())
}

/// Removes the query and fragment.
pub(crate) fn path_only(path: &str) -> &str {
    let end = path.find(['?', '#']).unwrap_or(path.len());
    &path[..end]
}

/// Checks for anything a server could decode or normalize into a path other
/// than the one compared: dot segments, encoded dots and slashes, double
/// encoding, backslashes, empty segments and `;` path parameters.
fn is_unambiguous_path(path: &str) -> bool {
    if !is_valid_path(path) || path.contains(['\\', ';']) || path.contains("//") {
        return false;
    }
    let lower = path.to_ascii_lowercase();
    if ["%2e", "%2f", "%5c", "%25", "%00", "%3b"]
        .iter()
        .any(|needle| lower.contains(needle))
    {
        return false;
    }
    !path
        .split('/')
        .any(|segment| segment == "." || segment == "..")
}

/// A prefix for a rule or binding must itself be an unambiguous path.
pub(crate) fn is_valid_path_prefix(prefix: &str) -> bool {
    is_unambiguous_path(prefix) && !prefix.contains(['?', '#'])
}

/// True if `path` is under `prefix` on a segment boundary, so `/v1/messages`
/// matches `/v1/messages` and `/v1/messages/x` but not `/v1/messagesx`.
/// A path that is ambiguous in any way does not match.
pub(crate) fn path_has_prefix(path: &str, prefix: &str) -> bool {
    // A fragment is never sent in a real request target, so one is suspect.
    if path.contains('#') {
        return false;
    }
    let path = path_only(path);
    if !is_unambiguous_path(path) {
        return false;
    }
    if prefix.ends_with('/') {
        return path.starts_with(prefix);
    }
    match path.strip_prefix(prefix) {
        Some(rest) => rest.is_empty() || rest.starts_with('/'),
        None => false,
    }
}
