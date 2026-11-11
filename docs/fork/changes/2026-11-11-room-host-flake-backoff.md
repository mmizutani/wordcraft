# A pull request no longer fails the gate because two agents and the window type into one paragraph at once

- **Issue:** none; seen failing the gate on #138 and #139
- **Upstream doc it would have touched:** none

`hosted_copies_on_threads_and_the_window_typing_at_once_end_up_the_same` failed about one run in four. A `text.newParagraph` takes away the block it splits, so it clashes with any key typed into that paragraph since the copy's version. With the window typing between frames, the hub sent it back `MAX_SENDS` times in a row, and `hosted::call` answered with its documented error. The test treated that answer as fatal.

The room's behaviour is unchanged. In 80 instrumented runs, all 421 send-backs were clashes with a change the hub had accepted since. The test now carries on as the error says to, and settles every copy before comparing them.

The test also compares the whole accepted text now. The old count of each agent's `{i}9` could not see a paragraph break that was lost, and a paragraph break is what this back-off usually holds up.

The gate had a second cause, in production code. The room's port wrote each reply straight to the socket, so every JSON token was its own write and, with `TCP_NODELAY`, its own packet. A long document's `document.inspect` reply ran out the port's 2-second write deadline part-way, and the agent got a cut-off line. That failed `a_person_and_three_agents_edit_a_long_document_at_once_with_no_stalls` on #138 and #142. The port now serializes a reply first and writes it in one piece, under the same deadline.
