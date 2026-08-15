---
title: Index
tags: [home]
---

# Heading One

Welcome. Start with [[projects/rust-app]] or read the [[Readme]].

Today's log: [[2026-08-14]].

This one doesn't exist yet: [[missing-note]].

A classic markdown link (not a wikilink, not an edge in v1): [ideas](projects/ideas.md).

Embedded image (embed + non-md target, skipped in v1): ![[diagram.png]]

Embedded note (md-resolvable target — if embed handling regressed, this would
surface loudly as a ghost node): ![[embedded-note-trap]]

A plain wikilink to an image resolves to its Image node: [[diagram.png]]

```text
This fenced block mentions [[trap-link]] and must NOT create an edge.
```

Inline code: `[[inline-trap]]` must not create an edge either.

Source citations, wiki-style: [^notes/readme.md] and by name [^readme].
A real footnote [^1] stays a footnote.

[^1]: plain footnotes are untouched.
