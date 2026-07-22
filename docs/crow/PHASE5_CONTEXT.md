# Phase 5: Notes Browser Panel

## Goal

Add a `PanelKind::NotesBrowser` panel that shows `~/.crow/notes/` as a file tree. Clicking a file opens it in the editor. This gives agents a visible "memory" panel in the IDE.

## Architecture

Follow the same pattern as `PanelKind::TypstPreview` (which was just added). The notes browser is a simple panel — no proxy RPC needed, just filesystem reads.

### Files to create

**`lapce-app/src/panel/notes_browser.rs`** (~150 LOC)

```rust
pub struct NotesBrowserData {
    pub root: PathBuf,  // ~/.crow/notes/
    pub tree: RwSignal<Vec<NotesNode>>,
    pub expanded: RwSignal<im::HashSet<PathBuf>>,
}

pub struct NotesNode {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub depth: usize,
    pub children: Vec<NotesNode>,  // only populated when expanded
}
```

- On creation, scan `~/.crow/notes/` recursively (skip hidden files/dirs)
- Sort: directories first, then files, alphabetical within each
- Clicking a directory toggles expand/collapse
- Clicking a file opens it via `InternalCommand::OpenFile { path }`

**View function:**

```rust
pub fn notes_browser_panel(
    data: &NotesBrowserData,
    window_tab_data: Rc<WindowTabData>,
    _position: PanelPosition,
) -> impl View {
    // Header: "Notes" label + refresh button
    // Body: scrollable list of tree nodes
    // Each node: indent by depth, folder/file icon, name
    // Click handler: toggle expand (dir) or open file (file)
}
```

### Files to modify

**`lapce-app/src/panel/kind.rs`**
- Add `NotesBrowser` variant to `PanelKind`

**`lapce-app/src/panel/data.rs`**
- Add `NotesBrowser` to `default_panel_order` (BottomLeft, alongside Chat)

**`lapce-app/src/panel/view.rs`**
- Add `NotesBrowser` to `panel_view` match → call `notes_browser_panel()`
- Add `NotesBrowser` to `panel_picker` (the panel tab icons)

**`lapce-app/src/window_tab.rs`**
- Add `notes_browser: NotesBrowserData` field to `WindowTabData`
- Initialize in `WindowTabData::new()`

**`lapce-app/src/config/icon.rs`**
- Add `NOTES_BROWSER` constant (reuse the `FILE` icon or `book.svg` if available)

**`defaults/icon-theme.toml`**
- Register the icon if using a new one

### Tree rendering

Use a flat list with depth-based indentation (simpler than recursive views):

```rust
fn flatten_tree(nodes: &[NotesNode], expanded: &im::HashSet<PathBuf>) -> Vec<(PathBuf, String, bool, usize)> {
    let mut result = Vec::new();
    for node in nodes {
        result.push((node.path.clone(), node.name.clone(), node.is_dir, node.depth));
        if node.is_dir && expanded.contains(&node.path) {
            result.extend(flatten_tree(&node.children, expanded));
        }
    }
    result
}
```

Render as a `list` or `stack` of rows, each with:
- Left padding: `depth * 16.0` px
- Icon: folder (▶/▼) or file (📄)
- Label: filename

### Opening files

When a file node is clicked:
```rust
window_tab_data.common.internal_command.send(
    InternalCommand::OpenFile { path: node_path }
);
```

### Refresh

Add a refresh button (↻) in the header that re-scans the directory:
```rust
fn scan_notes_dir(root: &Path) -> Vec<NotesNode> {
    // Recursive scan, skip hidden files (starting with .)
    // Sort: dirs first, then files, alphabetical
}
```

### What NOT to do

- Don't use the existing FileExplorerData — it's too complex and tied to the workspace
- Don't add file watching (inotify) — manual refresh is fine
- Don't add editing capabilities — this is read-only browsing
- Don't add search — keep it simple

### Build verification

```bash
cd ~/src/crow-team/lapce && cargo check 2>&1 | tail -5
```

Must compile with 0 errors.
