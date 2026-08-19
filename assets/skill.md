---
name: text-graph
description: Working inside a text-graph vault — a folder of notes shown as a live graph, where other agents may be working at the same time. Use when the working directory or an ancestor holds a `.text-graph/` directory, when notes link each other with [[wikilinks]], or when you need to see, message or read another agent in the same vault (`text-graph roster` / `send` / `peek`).
---

# Working in a text-graph vault

This folder is a **graph**, and someone is probably looking at it. text-graph
renders the vault as nodes and edges — live, reloading as files change — so the
structure you leave behind is something a human reads at a glance, and possibly
something another agent is editing at the same moment.

Two things follow: write so the graph stays legible, and check who else is here
before assuming you are alone.

## The vault is a graph

- **Every visible file is a node.** Markdown files, images, and any other file
  (its name keeps the extension). Hidden files, `node_modules/`, `target/` and
  `__pycache__/` are skipped.
- **Folders are the tree.** A file's parent folder is its parent node.
- **`[[wikilinks]]` are edges**, Obsidian-flavored: `[[note]]`, `[[note|label]]`,
  `[[folder/note#heading]]`, `![[embedded note]]`. Frontmatter `aliases:` resolve
  too, so a note can be reached by more than one name.
- **A link to something that doesn't exist yet becomes a ghost node** — drawn
  hollow, listing everyone who linked it. A ghost is not an error; it is a
  standing invitation to write that note.
- **URLs in notes become web nodes**, deduplicated across the vault. Two notes
  citing the same source are visibly connected even when neither links the other.

What this asks of you: **link generously and deliberately**. A note nobody links
to is an island in the picture the human is reading. When you write a
conclusion, link it from the notes it concerns; when you cite a source, cite the
URL in the note rather than only in your reply.

## Other agents may be working here

Each agent running in this vault under tmux appears as a live terminal card in
the graph, and can be reached directly:

```
text-graph roster                  who else is live, and how long they've been quiet
text-graph send <agent> <message>  type a message into another agent's terminal
text-graph peek <agent> [-n N]     read the last N lines of their screen
text-graph protocol                print this skill's text
```

None of them take a vault path — the vault is found from the working directory,
git-style. A typical look around:

```
$ text-graph roster
AGENT   SESSION      QUIET  WHERE           LAST LINE
claude  tg_claude    3s     wiki/concepts   writing sources/pinned-memory.md
pi      tg_pi (you)  0s     raw
```

Address an agent by its harness name (`claude`), its session (`tg_claude`) or
its pane id (`%3`). A name matching two sessions fails rather than guessing —
use the session name to disambiguate. Sending is refused for shell and editor
cards, because a message pasted into a shell would run as a command.

A message arrives as if typed at the other agent's prompt, prefixed with who
sent it and how to answer. If they are mid-turn their harness queues it; you do
not have to wait for them to go quiet.

## Conventions

1. **Chatter goes in terminals; conclusions go in the vault.** A message is a
   nudge, not a record. Anything that should outlive the session is a note,
   linked from the notes it concerns — that is the shared memory, and it is the
   only memory that survives the session ending.
2. **Address a note, not an inbox.** For anything substantial, write the note
   first and send the link:
   `text-graph send pi "see [[topics/api-shape]] — need your read on §2"`.
3. **Do not re-send.** If a reply hasn't appeared, `peek` at them before pinging
   again: they may be deep in a tool call with your message already queued.
4. **Judge relevance yourself.** Nothing here routes, filters or summarizes —
   you are the router. If a message doesn't concern you, say so in one line or
   ignore it.
5. **Keep messages short.** Anything over 8 KiB is refused: that is a note.
6. **Reply the way you were reached.** The sender's name is in the prefix of the
   message you received.

## Reading the vault without the viewer

```
text-graph stats <vault-path>
```

Counts of nodes by kind and edges by kind, the ghosts and who conjured them,
ambiguous links, and the largest folders — a headless read of the same graph the
viewer draws. Useful before a restructure, and for checking afterwards that you
didn't strand anything.

## What text-graph will not do

- **It never edits your notes.** It writes only files you explicitly create from
  its menu, a hidden `.text-graph/` state directory, and this skill file when
  asked (`text-graph protocol --install`).
- **The viewer never types into a terminal.** Only agents and the human do, so
  anything that appears in a pane was sent by someone.
- **There is no daemon, bus or inbox.** Messages are terminal text and memory is
  the vault; every layer is a file or a screen you can look at.
