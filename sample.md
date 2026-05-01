# md-tui sample

A small document for **manual smoke-testing** the renderer.

## Features

- Mouse wheel scrolling
- *Italic*, **bold**, ~~strike~~, and `inline code`
- [External link](https://github.com/charmbracelet/glow)
- [Anchor link](#code) jumps to the **Code** heading
- [Sibling doc](./README.md) — navigates if present

> Block quotes
> with multiple lines
> and **emphasis** inside.

### Lists

1. First item
2. Second item with a [link](https://example.com)
3. Nested:
   - apples
   - oranges
   - [bananas](https://en.wikipedia.org/wiki/Banana)

- [x] Completed task
- [ ] Open task

## Code

```rust
fn main() {
    println!("hello, md-tui");
}
```

```python
def greet(name: str) -> str:
    return f"hello, {name}"
```

---

## Long paragraph

The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the
lazy dog. The quick brown fox jumps over the lazy dog. The quick brown fox jumps
over the lazy dog. The quick brown fox jumps over the lazy dog.

| Col A | Col B |
| --- | --- |
| 1 | one |
| 2 | two |
