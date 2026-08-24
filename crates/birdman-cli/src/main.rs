use birdman_client::Client;
use birdman_store::{FolderId, MessageSummary};

mod attach;
mod auth;
mod daemon;
mod format;
mod outbox;
mod send;
mod write;

fn main() {
    // Rust ignores SIGPIPE, so `birdman ls | head` panics instead of stopping.
    //
    // SAFETY: called once, before any thread exists.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };

    let args: Vec<String> = std::env::args().skip(1).collect();
    let (flags, positional): (Vec<_>, Vec<_>) = args
        .iter()
        .map(String::as_str)
        .partition(|a| a.starts_with("--"));

    let json = flags.contains(&"--json");
    let command = positional.first().copied().unwrap_or("help");

    let code = match command {
        "help" | "-h" | "--help" => {
            usage();
            0
        }
        "accounts" => run(json, commands::accounts),
        "folders" => run(json, |svc, json| {
            commands::folders(svc, json, flag_value(&args, "--account"))
        }),
        "ls" => run(json, |svc, json| {
            commands::list(
                svc,
                json,
                flag_value(&args, "--folder"),
                flags.contains(&"--unread"),
                flags.contains(&"--attachments"),
                flag_value(&args, "--limit")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(20),
            )
        }),
        "search" => match positional.get(1) {
            Some(text) => run(json, |svc, json| commands::search(svc, json, text)),
            None => {
                eprintln!("usage: birdman search <text>");
                1
            }
        },
        "login" => match auth::resolve(positional.get(1).copied(), "login") {
            Ok(account) => auth::login(&account),
            Err(code) => code,
        },
        "authorize" => match auth::resolve(positional.get(1).copied(), "authorize") {
            Ok(account) => auth::authorize(&account),
            Err(code) => code,
        },
        "check-auth" => match auth::resolve(positional.get(1).copied(), "check-auth") {
            Ok(account) => auth::check(&account),
            Err(code) => code,
        },
        // Before `run`: `stop` must work on a daemon whose protocol this build
        // cannot speak.
        "daemon" => match positional.get(1).copied() {
            Some("status") | None => daemon::status(),
            Some("stop") => daemon::stop(),
            Some("restart") => daemon::restart(),
            Some(other) => {
                eprintln!("unknown: birdman daemon {other}");
                eprintln!("usage: birdman daemon [status|stop|restart]");
                1
            }
        },
        "watch" => run(json, send::watch),
        "outbox" => match positional.get(1).copied() {
            None => run(json, outbox::list),
            Some("retry") => match positional.get(2).and_then(|v| v.parse().ok()) {
                Some(id) => run(json, |c, _| outbox::retry(c, birdman_store::OutboxId(id))),
                None => {
                    eprintln!("usage: birdman outbox retry <id>");
                    1
                }
            },
            Some("cancel") => match positional.get(2).and_then(|v| v.parse().ok()) {
                Some(id) => run(json, |c, _| outbox::cancel(c, birdman_store::OutboxId(id))),
                None => {
                    eprintln!("usage: birdman outbox cancel <id>");
                    1
                }
            },
            Some(other) => {
                eprintln!("unknown: birdman outbox {other}");
                eprintln!("usage: birdman outbox [retry|cancel] [<id>]");
                1
            }
        },
        "log" => daemon::log(
            flag_value(&args, "--lines")
                .and_then(|v| v.parse().ok())
                .unwrap_or(50),
            flags.contains(&"--follow"),
        ),
        "send" => match (flag_value(&args, "--to"), flag_value(&args, "--subject")) {
            (Some(to), Some(subject)) => run(json, |c, _| {
                send::send(
                    c,
                    flag_value(&args, "--from"),
                    to,
                    flag_value(&args, "--cc"),
                    subject,
                    flag_value(&args, "--body"),
                )
            }),
            _ => {
                eprintln!("usage: birdman send --to <addr> --subject <text> [--from <account>]");
                eprintln!("                   [--cc <addr>] [--body <text>|-]");
                eprintln!("the body is read from stdin unless --body is given");
                1
            }
        },
        "sync" => run(json, |c, _| write::sync(c, flag_value(&args, "--folder"))),
        "flag" | "unflag" => match message_arg(&positional) {
            Ok(id) => run(json, |c, _| write::flag(c, id, command == "flag")),
            Err(code) => code,
        },
        "archive" => match message_arg(&positional) {
            Ok(id) => run(json, |c, _| write::archive(c, id)),
            Err(code) => code,
        },
        "delete" => match message_arg(&positional) {
            Ok(id) => run(json, |c, _| write::delete(c, id)),
            Err(code) => code,
        },
        "move" => match (message_arg(&positional), positional.get(2)) {
            (Ok(id), Some(folder)) => run(json, |c, _| write::move_to(c, id, folder)),
            (Ok(_), None) => {
                eprintln!("usage: birdman move <message-id> <folder>");
                1
            }
            (Err(code), _) => code,
        },
        "contacts" => run(json, |svc, json| {
            attach::contacts(
                svc,
                json,
                flag_value(&args, "--limit")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(50),
            )
        }),
        "attachments" => match message_arg(&positional) {
            Ok(id) => run(json, |svc, json| {
                attach::list(svc, json, id, flag_value(&args, "--save"))
            }),
            Err(code) => code,
        },
        "mark" => match (message_arg(&positional), positional.get(2).copied()) {
            (Ok(id), Some(state @ ("read" | "unread"))) => {
                run(json, |c, _| write::mark_seen(c, id, state == "read"))
            }
            (Ok(_), _) => {
                eprintln!("usage: birdman mark <message-id> read|unread");
                1
            }
            (Err(code), _) => code,
        },
        "reply" => match message_arg(&positional) {
            Ok(id) => run(json, |c, _| {
                send::reply(c, id, flags.contains(&"--all"), flag_value(&args, "--body"))
            }),
            Err(code) => code,
        },
        "forward" => match (message_arg(&positional), flag_value(&args, "--to")) {
            (Ok(id), Some(to)) => run(json, |c, _| {
                send::forward(c, id, to, flag_value(&args, "--body"))
            }),
            (Ok(_), None) => {
                eprintln!("usage: birdman forward <message-id> --to ADDR [--body TEXT]");
                1
            }
            (Err(code), _) => code,
        },
        "read" => match positional.get(1).and_then(|v| v.parse().ok()) {
            Some(id) => {
                let id = birdman_store::MessageId(id);
                let peek = flags.contains(&"--peek");
                run(json, move |svc, json| {
                    write::open(svc, id, peek)?;
                    commands::read(svc, json, id)
                })
            }
            None => {
                eprintln!("usage: birdman read <message-id> [--peek]");
                1
            }
        },
        other => {
            eprintln!("unknown command: {other}");
            usage();
            1
        }
    };
    std::process::exit(code);
}

fn message_arg(positional: &[&str]) -> Result<birdman_store::MessageId, i32> {
    match positional.get(1).and_then(|v| v.parse().ok()) {
        Some(id) => Ok(birdman_store::MessageId(id)),
        None => {
            eprintln!(
                "usage: birdman {} <message-id>",
                positional.first().unwrap_or(&"<command>")
            );
            Err(1)
        }
    }
}

fn flag_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if let Some(rest) = arg.strip_prefix(name) {
            if let Some(value) = rest.strip_prefix('=') {
                return Some(value);
            }
            if rest.is_empty() {
                return iter.next().map(String::as_str);
            }
        }
    }
    None
}

fn run(json: bool, body: impl FnOnce(&Client, bool) -> Result<(), String>) -> i32 {
    let client = match Client::connect() {
        Ok(client) => client,
        Err(err) => {
            eprintln!("{err}");
            return 1;
        }
    };
    match body(&client, json) {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("{err}");
            1
        }
    }
}

fn usage() {
    println!(
        "\
birdman -- command-line client for the Birdman mailbox

USAGE
    birdman <command> [options]

COMMANDS
    accounts                 configured accounts
    folders [--account ID]   folders, in sidebar order
    ls [--folder NAME]       messages, newest first
       [--unread] [--attachments] [--limit N]
    search <text>            full-text search
    read <id> [--peek]       one message's body, fetching it if needed
    attachments <id>         what a message carries
       [--save DIR]          write copies out under their real names
    contacts [--limit N]     everyone you have corresponded with

    sync [--folder NAME]     fetch new mail (every inbox by default)
    mark <id> read|unread    set or clear the seen mark
    flag / unflag <id>       set or clear the flagged mark
    move <id> <folder>       move to another folder on the same account
    archive <id>             move to the account's archive
    delete <id>              move to trash
    send --to A --subject S  send a message (body from stdin)
    reply <id> [--all]       answer one, threaded and quoted
    forward <id> --to A      pass one on
    outbox                   mail queued for delivery
    outbox retry <id>        requeue a failed send now
    outbox cancel <id>       drop a queued send
    watch                    print changes as they happen
    log [--lines N] [--follow]
                             the daemon's log, which is where sync fails

    login <account>          store an account's password in the keyring
    authorize <account>      grant OAuth2 consent (once, per account)
    check-auth <account>     resolve a credential and try the login
    daemon [status|stop|restart]

OPTIONS
    --json                   machine-readable output

Everything goes through birdmand, which starts on demand and stops when idle."
    );
}

mod commands {
    use super::*;

    pub fn accounts(service: &Client, json: bool) -> Result<(), String> {
        let accounts = service.accounts().map_err(|e| e.to_string())?;
        if json {
            println!(
                "[{}]",
                accounts
                    .iter()
                    .map(format::account_json)
                    .collect::<Vec<_>>()
                    .join(",")
            );
            return Ok(());
        }
        for account in accounts {
            println!(
                "{:<4} {:<24} {}",
                account.id.0, account.display_name, account.email
            );
        }
        Ok(())
    }

    pub fn folders(service: &Client, json: bool, account: Option<&str>) -> Result<(), String> {
        let account = match account {
            Some(raw) => Some(resolve_account(service, raw)?),
            None => None,
        };
        let folders = service.folders(account).map_err(|e| e.to_string())?;
        let unread: std::collections::HashMap<FolderId, u32> = service
            .unread_counts()
            .map_err(|e| e.to_string())?
            .into_iter()
            .collect();

        if json {
            println!(
                "[{}]",
                folders
                    .iter()
                    .map(|f| format::folder_json(f, unread.get(&f.id).copied().unwrap_or(0)))
                    .collect::<Vec<_>>()
                    .join(",")
            );
            return Ok(());
        }
        for folder in folders {
            let count = unread.get(&folder.id).copied().unwrap_or(0);
            let badge = if count > 0 {
                format!(" ({count})")
            } else {
                String::new()
            };
            println!("{:<5} {}{}", folder.id.0, folder.imap_path, badge);
        }
        Ok(())
    }

    pub fn list(
        service: &Client,
        json: bool,
        folder: Option<&str>,
        unread_only: bool,
        attachments_only: bool,
        limit: u32,
    ) -> Result<(), String> {
        let folders = service.folders(None).map_err(|e| e.to_string())?;
        let selected: Vec<FolderId> = match folder {
            Some(name) => {
                let matches: Vec<_> = folders
                    .iter()
                    .filter(|f| {
                        f.imap_path.eq_ignore_ascii_case(name) || f.name.eq_ignore_ascii_case(name)
                    })
                    .map(|f| f.id)
                    .collect();
                if matches.is_empty() {
                    return Err(format!(
                        "no folder matching {name:?} -- try `birdman folders`"
                    ));
                }
                matches
            }
            None => folders
                .iter()
                .filter(|f| f.imap_path.eq_ignore_ascii_case("INBOX"))
                .map(|f| f.id)
                .collect(),
        };

        let messages = service
            .messages(
                selected,
                None,
                limit,
                birdman_store::MessageFilter {
                    unread: unread_only,
                    attachments: attachments_only,
                },
            )
            .map_err(|e| e.to_string())?;
        print_messages(&messages, json);
        Ok(())
    }

    pub fn search(service: &Client, json: bool, text: &str) -> Result<(), String> {
        let messages = service
            .search(text, birdman_store::MessageFilter::default(), 50)
            .map_err(|e| e.to_string())?;
        print_messages(&messages, json);
        Ok(())
    }

    pub fn read(
        service: &Client,
        json: bool,
        message: birdman_store::MessageId,
    ) -> Result<(), String> {
        let body = service
            .body(message)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("message {} has no cached body yet", message.0))?;

        if json {
            println!("{}", format::body_json(&body));
            return Ok(());
        }
        // The store's "text" column is not reliably text: `mail-parser` puts
        // the HTML part there when a message has no plain alternative.
        let text = body
            .text
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty());
        match text {
            Some(text) if !format::looks_like_html(text) => println!("{text}"),
            Some(markup) => println!("{}", format::html_to_text(markup)),
            None => match body.html.as_deref() {
                Some(html) => println!("{}", format::html_to_text(html)),
                None => println!("(empty)"),
            },
        }
        Ok(())
    }

    fn print_messages(messages: &[MessageSummary], json: bool) {
        if json {
            println!(
                "[{}]",
                messages
                    .iter()
                    .map(format::message_json)
                    .collect::<Vec<_>>()
                    .join(",")
            );
            return;
        }
        for message in messages {
            println!(
                "{:<7} {} {:<28} {}",
                message.id.0,
                format::when(message.date),
                format::truncate(
                    message
                        .from_name
                        .as_deref()
                        .or(message.from_addr.as_deref())
                        .unwrap_or(""),
                    28
                ),
                format::truncate(message.subject.as_deref().unwrap_or("(no subject)"), 60),
            );
        }
    }

    fn resolve_account(service: &Client, raw: &str) -> Result<birdman_store::AccountId, String> {
        let accounts = service.accounts().map_err(|e| e.to_string())?;
        if let Ok(id) = raw.parse::<i64>() {
            if accounts.iter().any(|a| a.id.0 == id) {
                return Ok(birdman_store::AccountId(id));
            }
        }
        let needle = raw.to_lowercase();
        let matches: Vec<_> = accounts
            .iter()
            .filter(|a| {
                a.email.to_lowercase().starts_with(&needle)
                    || a.display_name.to_lowercase().starts_with(&needle)
            })
            .collect();
        match matches.as_slice() {
            [one] => Ok(one.id),
            [] => Err(format!("no account matching {raw:?}")),
            several => Err(format!(
                "{raw:?} matches {} accounts: {}",
                several.len(),
                several
                    .iter()
                    .map(|a| a.email.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }
}
