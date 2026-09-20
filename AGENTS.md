# Conversational style / development style

* Tell me what's happening plainly as you work on it. Feel free to be technical, but keep things to the point.

* While measuring and testing is good, pay attention to how much time these things take to run. Don't feel the need to benchmark or test every single commit - failures will be caught sooner or later.  Or run tests and benchmarks async and don't block progress on them.

# Coding style

* Write elegant, concise, efficient code. The priority is performance over elegance, though.

* Pay attention to the existing style of the repo and mimic it.

* Prioritize code clarity and correctness. After that comes performance / lightweight-ness. This is a tradeoff. Clever optimizations are fine when they can be put behind a clear and understandable abstraction.

* Prefer implementing functionality in existing files unless it is a new logical component. Avoid creating many small files.

* Concise code should still be easy to scan. Use blank lines to separate logical groups, avoid packing multiple statements onto one line, and run `cargo fmt` for Rust changes.

* Avoid creative additions unless explicitly requested

* Use full words for variable names (no abbreviations like "q" for "queue")

# Comment style

This gets its own section because it's very important.

* Assume that the reader is a skilled software engineer that has context on the codebase

* Write down things that would not be obvious to the reader:

  - If some code is non-obvious or surprising, explain why it is that way. Do **not** give a story about the sequence of events that led to the code. Some narrative is sometimes ok, but the focus is on the timeless why.

  - If some invariant needs to be preserved

  - Do not write down the details of design decisions, that's what the design docs are for.

* Avoid fancy language and superfluous words. Removing unnecessary words is very
  good. Lets acknowledge and respect the cost to the downstream reader (human or
  AI).

* Comments are not capitalized when they are a fragment. They are capitalized
  when they are a complete sentence.

# Documents

Documents should have similar concision and simplicity as comments. However, they can go into a lot more comprehensive details. Even so the principle of using fewer words when possible to convey the information holds.

# Commit style

The body should be concise when possible, but also going into details that might be relevant to someone ending up on the commit via a blame in the future. These do not need to be PR style descriptions - these do not need to advocate for the change or persuade.
