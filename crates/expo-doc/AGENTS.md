# expo-doc

HTML documentation generator from `@doc` annotations.

## Key files

- `extract.rs` -- Walks AST, builds doc structs from `@doc` annotations and declarations
- `render.rs` -- Askama HTML templates for index and per-type pages. Markdown rendering via pulldown-cmark
- `style.rs` -- Embeds `templates/style.css`
