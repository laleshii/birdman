---
id: multi-account-ui
title: 'Multiple accounts: one at a time, or merged'
altitude: 2
topics:
- ui
relations:
- type: part_of
  target: gpui-application
- type: references
  target: floating-overlays
summary: The sidebar switcher and the merged all-accounts view, how a merged folder row expands to many, and the scoping rules that stop one account's actions landing on another.
---

# Multiple accounts: one at a time, or merged

The sidebar shows **one scope** at a time, and its label is the switcher.

```rust
pub enum AccountScope { All, One(AccountId), Unset }
```

Stacking every account's folders was the first attempt, and it made the
sidebar's length grow with the account count -- a scroll before the second
account was fully synced.

## The merged view

`AccountScope::All` shows only the **default** folders, each row standing for
that folder on every account. A unified Inbox is meaningful; a union of two
accounts' custom label trees is not.

The merge was cheap because of something already true: `sidebar_folder_rank`
gives default folders a stable identity across accounts (Inbox 0, Flagged 1,
Drafts 2, Sent 3, Trash 4), and the store's paging, counting and search already
take a **slice** of folder ids for nested folder trees.

So merging is one function widening that slice:

```rust
fn selected_folder_ids(&self, folder_id) -> Vec<FolderId>
```

In the merged view a default row returns every folder of the same rank; nested
trees behave as before. Paging, counts and infinite scroll follow for free.

"Show more" still works there, grouped per account -- one collapsible, several
labelled groups.

## Scoping is the part that bites

Anything that picks a *target* must be scoped to an account, and the failure
mode is worse than cosmetic.

- **`special_folder(account, use)`** takes an account. With two accounts,
  `self.folders` holds two Trash folders, and archiving a work message into a
  personal Trash is real data loss.
- **Actions route by the message, not the selection.** Archive, delete and flag
  resolve the account from `msg.folder_id`, so a merged inbox cannot misfile.
- **Move targets** are the message's own account's folders. Moving between two
  servers is not a move -- it would be a copy and a delete, which this does not
  do.
- **Sync covers the whole merged set.** `sync_now` syncs every folder the
  current row represents, re-listing once per account. Syncing one inbox while
  showing three leaves the view silently stale.
- **Switching accounts clears search results.** `visible_messages` prefers
  them, so results from the account being left would otherwise sit over the new
  account's inbox.
- **Switching selects the new account's inbox.** The previously selected folder
  belongs to the account being left; keeping it shows one account's name above
  another's mail.

## The switcher is a picker

It was click-only for a while, which made "Switch account" the one command in
the palette that then required a mouse -- a keyboard-first app with a
keyboard-shaped hole in it.

It now uses the same contract as the move picker and the palette
([[picker-component]]): arrows to move, typing to filter, Enter to choose,
Escape to dismiss. The option list is built in `AppState` rather than in the
renderer, so the keyboard and the mouse choose from exactly the same list --
otherwise a highlight index means nothing.

## Unread bubbles

Only the default folders get one. A count on every row turns the sidebar into a
wall of numbers, and for a Gmail label tree most of them duplicate what All
Mail already counted.

Counts come from `Store::unread_counts()` -- a single grouped query, refreshed
with the folder list. One `count_messages` per folder would be thirty round
trips through the store mutex that the sync engine is also contending for.

In the merged view a bubble sums the same expansion `selected_folder_ids` uses,
so the number never disagrees with the list it opens.

## Single-account is unchanged

`visible_folders()` returns everything when there is one account, and the header
stays a plain label. None of the above is visible until a second account
exists, which is deliberate -- and is also why this class of bug survived so
long: **an id-collision or scoping mistake is invisible with one account.**
