use birdman_config::{Config, ConfiguredAccount};

pub fn resolve(account_id: Option<&str>, verb: &str) -> Result<ConfiguredAccount, i32> {
    let accounts = match birdman_config::load() {
        Config::Accounts(accounts) => accounts,
        Config::Unconfigured { path, error } => {
            eprintln!("No usable account in {}", path.display());
            if let Some(error) = error {
                eprintln!("  {error}");
            }
            return Err(1);
        }
    };
    let found = match account_id {
        Some(id) => accounts.iter().find(|a| a.id == id),
        None if accounts.len() == 1 => accounts.first(),
        None => None,
    };
    match found {
        Some(account) => Ok(account.clone()),
        None => {
            eprintln!("usage: birdman {verb} <account>");
            eprintln!(
                "configured accounts: {}",
                accounts
                    .iter()
                    .map(|a| a.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            Err(1)
        }
    }
}

pub fn check(account: &ConfiguredAccount) -> i32 {
    let adapter = account.auth.adapter();
    let ctx = birdman_auth::AuthContext {
        account_id: account.id.clone(),
        username: account.auth.username.clone(),
    };
    println!("account   {}", account.id);
    println!("username  {}", account.auth.username);
    println!("adapter   {}", adapter.name());

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build a runtime");
    match runtime.block_on(adapter.credentials(&ctx)) {
        Ok(credentials) => {
            match &credentials {
                birdman_auth::Credentials::Password(_) => {
                    println!("result    ok -- a password was resolved")
                }
                birdman_auth::Credentials::OAuth2 {
                    username,
                    access_token,
                } => {
                    println!("result    ok -- an access token was issued for {username}");
                    println!("token     {} chars", access_token.len());
                }
            }

            println!();
            println!(
                "imap      {}:{}",
                account.receiver.host, account.receiver.port
            );
            let probe = birdman_imap::connect_and_authenticate(
                &account.receiver.host,
                account.receiver.port,
                &credentials,
                &account.auth.username,
                account.danger_accept_invalid_certs,
            );
            match runtime.block_on(async {
                tokio::time::timeout(std::time::Duration::from_secs(30), probe).await
            }) {
                Ok(Ok(mut session)) => {
                    println!("login     ok -- the server accepted the credential");
                    match runtime.block_on(birdman_imap::list_folder_paths(&mut session)) {
                        Ok(paths) => {
                            println!("folders   {} listed by the server", paths.len());
                            for path in paths {
                                println!("          {path}");
                            }
                        }
                        Err(err) => println!("folders   FAILED: {err}"),
                    }
                    0
                }
                Ok(Err(err)) => {
                    println!("login     FAILED");
                    println!("          {err}");
                    1
                }
                Err(_) => {
                    println!("login     TIMED OUT after 30s");
                    1
                }
            }
        }
        Err(err) => {
            println!("result    FAILED");
            println!("          {err}");
            1
        }
    }
}

pub fn authorize(account: &ConfiguredAccount) -> i32 {
    let birdman_config::AuthKind::OAuth2 {
        endpoints,
        client_id,
        client_secret,
    } = &account.auth.kind
    else {
        eprintln!(
            "Account {:?} does not use OAuth2 (auth.type is not \"oauth2\"), so there is nothing to authorize.",
            account.id
        );
        return 1;
    };

    let pending = match birdman_auth::begin_authorization(
        endpoints.clone(),
        client_id,
        client_secret.clone(),
        Some(&account.auth.username),
    ) {
        Ok(pending) => pending,
        Err(err) => {
            eprintln!("Could not start authorization: {err}");
            return 1;
        }
    };

    println!("Authorizing {} ({})", account.id, account.auth.username);
    println!();
    println!("Opening your browser. If it doesn't open, visit:");
    println!();
    println!("  {}", pending.authorize_url);
    println!();
    // Printed first, so a headless machine is still usable.
    let _ = open::that_detached(&pending.authorize_url);
    println!("Waiting for the redirect...");

    match pending.wait_for_refresh_token() {
        Ok(refresh_token) => {
            match birdman_auth::store_refresh_token(
                birdman_config::KEYRING_SERVICE,
                &account.auth.username,
                &refresh_token,
            ) {
                Ok(()) => {
                    println!(
                        "Authorized. The refresh token is in your keyring; start Birdman normally."
                    );
                    0
                }
                Err(err) => {
                    eprintln!("Authorized, but saving the refresh token failed: {err}");
                    1
                }
            }
        }
        Err(err) => {
            eprintln!("Authorization failed: {err}");
            1
        }
    }
}

pub fn login(account: &ConfiguredAccount) -> i32 {
    if !account.auth.is_prompted() {
        eprintln!(
            "Account {:?} does not use the keyring (auth.type is not \"keyring\"), so there is no \
             password to store.",
            account.id
        );
        if matches!(account.auth.kind, birdman_config::AuthKind::OAuth2 { .. }) {
            eprintln!("Run `birdman authorize {}` instead.", account.id);
        }
        return 1;
    }

    println!("Password for {} ({})", account.auth.username, account.id);
    if account.receiver.host.ends_with("gmail.com") {
        println!("Gmail needs an App Password, not your normal login password:");
        println!("  https://myaccount.google.com/apppasswords");
    }
    let password = match rpassword::prompt_password("password: ") {
        Ok(password) => password,
        Err(err) => {
            eprintln!("could not read the password: {err}");
            return 1;
        }
    };
    if password.is_empty() {
        eprintln!("nothing entered; leaving the keyring alone");
        return 1;
    }

    match birdman_auth::store_password(
        birdman_config::KEYRING_SERVICE,
        &account.auth.username,
        &password,
    ) {
        Ok(()) => {
            println!(
                "Saved. The daemon picks it up on its next reconnect -- `birdman check-auth {}`",
                account.id
            );
            println!("confirms it works.");
            0
        }
        Err(err) => {
            eprintln!("could not save to the keyring: {err}");
            1
        }
    }
}
