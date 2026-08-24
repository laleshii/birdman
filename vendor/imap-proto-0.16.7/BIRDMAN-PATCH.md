# Birdman compatibility patch

This is `imap-proto` 0.16.7 under its original MIT/Apache-2.0 license. The only
behavioral change allows zero or more spaces between a partial BODY response's
origin (`<0>`) and its literal. RFC 3501 requires one space, but GreenMail emits
none; rejecting the whole FETCH response prevents otherwise-valid mailboxes
from syncing. A regression test covers the tolerated response.

Remove this patch once the behavior is accepted upstream or Birdman's IMAP
stack no longer depends on this parser.
