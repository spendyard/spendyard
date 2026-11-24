mod cli;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};

/// Reads receipts and invoices and works out what you're paying for.
#[derive(Parser)]
#[command(name = "spendyard", version = spendyard_core::VERSION, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Read what documents say about money: vendor, amounts, card, seats,
    /// usage and billing. Exits 0 when a receipt or invoice was found, 1
    /// when none was, 2 on errors.
    Read {
        /// Emails (.eml, .msg), PDFs, images, HTML or text files; `-` reads
        /// standard input.
        #[arg(required = true)]
        inputs: Vec<PathBuf>,

        /// `provider` or `provider/model`: anthropic, openai, gemini or
        /// openai-compatible. Default: the config, then anthropic.
        #[arg(long, short)]
        extractor: Option<String>,

        /// The cheap first model: jev, clef, clef-flash, systemone or none.
        /// Default: the config, then none.
        #[arg(long, short)]
        decider: Option<String>,

        /// Print a readable summary instead of JSON.
        #[arg(long)]
        pretty: bool,

        /// One JSON object per line. The default for several inputs.
        #[arg(long)]
        jsonl: bool,

        /// How many documents to read at once.
        #[arg(long, default_value_t = 4)]
        concurrency: usize,

        /// Don't keep the results in ~/.spendyard/spendyard.db.
        #[arg(long)]
        no_store: bool,
    },

    /// Measure models: draft expected answers for a folder of documents,
    /// score model combinations against them, or ask the first model alone.
    Eval {
        #[command(subcommand)]
        action: EvalAction,
    },

    /// List what's in a document: the email body, attachments, forwarded
    /// emails. No model calls.
    Parts {
        /// An email (.eml, .msg), PDF, image, HTML or text file; `-` reads
        /// standard input.
        input: PathBuf,

        /// Print JSON instead of a summary.
        #[arg(long)]
        json: bool,

        /// Also print each part's text.
        #[arg(long)]
        text: bool,
    },

    /// Run as an MCP server over stdio, with read_document,
    /// decide_document and list_parts tools for agents.
    Mcp {
        #[arg(long, short)]
        extractor: Option<String>,

        #[arg(long, short)]
        decider: Option<String>,
    },

    /// Spendyard Cloud: sign this machine in, see where it acts, switch it
    /// to another organisation, sign it out.
    Cloud {
        #[command(subcommand)]
        action: CloudAction,
    },

    /// Mailboxes the CLI reads itself: sign in, list them, scan them,
    /// sign out.
    Mailbox {
        #[command(subcommand)]
        action: MailboxAction,
    },

    /// The documents in the store: list them, and correct one by hand.
    Documents {
        #[command(subcommand)]
        action: DocumentsAction,
    },

    /// Spending per vendor or per month, from the store, in one currency.
    Report {
        /// vendor or month.
        #[arg(long, default_value = "vendor")]
        by: String,

        /// How many months back.
        #[arg(long, default_value_t = 12)]
        months: u32,

        /// Currency to report in. Default: the one paid in most often.
        #[arg(long)]
        currency: Option<String>,

        #[arg(long)]
        json: bool,
    },

    /// Recurring vendors: cost per month, interval, seats, card, next
    /// renewal, and whether the last payment went through.
    Subscriptions {
        #[arg(long)]
        currency: Option<String>,

        #[arg(long)]
        json: bool,

        /// Also show subscriptions lapsed for more than a year.
        #[arg(long)]
        all: bool,
    },

    /// Every payment once, money paid or back, with its receipts, invoices
    /// and lines on card statements, newest first. Each keeps its number as
    /// its evidence changes.
    Payments {
        /// Only those with no receipt or invoice: a statement's line alone.
        #[arg(long)]
        missing: bool,

        /// Only this vendor (part of its name or domain).
        #[arg(long)]
        vendor: Option<String>,

        #[arg(long, default_value_t = 50)]
        limit: usize,

        #[arg(long)]
        json: bool,
    },

    /// Card payments read from statements (`read` reads them), matched to
    /// receipts: what's missing a receipt, and receipts with no payment.
    Transactions {
        #[command(subcommand)]
        action: Option<TransactionsAction>,
    },

    /// The vendor catalog: lookups, lists, and vendors in your store it
    /// doesn't know yet.
    Vendors {
        #[command(subcommand)]
        action: Option<VendorsAction>,
    },

    /// Show where the config lives, the models in effect and which keys
    /// are set.
    Config {
        #[command(subcommand)]
        action: Option<ConfigAction>,
    },
}

#[derive(Subcommand)]
enum DocumentsAction {
    /// List the receipts, invoices and failed payments in the store, newest first.
    List {
        /// Only this vendor (part of its name or domain).
        #[arg(long)]
        vendor: Option<String>,

        /// YYYY-MM-DD.
        #[arg(long)]
        since: Option<String>,

        #[arg(long, default_value_t = 50)]
        limit: usize,

        /// Everything stored, not only receipts, invoices and failed payments.
        #[arg(long)]
        all: bool,

        #[arg(long)]
        json: bool,
    },

    /// Show one: where it came from, how it was read and what was read,
    /// its corrections, and the payments it's evidence of.
    Get {
        /// The ID `spendyard documents list` shows.
        id: i64,

        /// Print it as JSON, the stored result whole.
        #[arg(long)]
        json: bool,
    },

    /// Correct a document by hand: personal, ignored, another vendor
    /// name. Edits survive reading it again.
    Edit {
        /// The ID `spendyard documents list` shows.
        id: i64,

        /// Paid privately: left out of spend.
        #[arg(long)]
        personal: bool,

        /// Not spend at all: left out everywhere.
        #[arg(long)]
        ignore: bool,

        /// Undo --personal and --ignore.
        #[arg(long)]
        restore: bool,

        /// Group it under this vendor name.
        #[arg(long)]
        vendor: Option<String>,

        #[arg(long)]
        note: Option<String>,
    },

    /// Browse them in the terminal: the list, the one picked whole, and
    /// corrections a key each.
    Ui,
}

#[derive(Subcommand)]
enum EvalAction {
    /// Score model combinations against the expected answers in a folder:
    /// accuracy per field, routing, flags, cost and time.
    Run {
        dir: PathBuf,

        /// First models to compare, comma-separated; `none` for no first
        /// model. Default: the config.
        #[arg(long, short)]
        decider: Option<String>,

        /// Extractors to compare, comma-separated. Default: the config.
        #[arg(long, short)]
        extractor: Option<String>,

        /// Only the first this many documents.
        #[arg(long)]
        limit: Option<usize>,

        #[arg(long, default_value_t = 6)]
        concurrency: usize,

        /// List every wrong field.
        #[arg(long)]
        wrong: bool,

        /// One JSON line per combination.
        #[arg(long)]
        json: bool,
    },

    /// Draft an expected answer for each document in a folder that has
    /// none, with a careful model, for a person to review.
    Label {
        dir: PathBuf,

        /// The model to draft with. Default: anthropic/claude-opus-5-5.
        #[arg(long, short)]
        extractor: Option<String>,

        /// Draft at most this many.
        #[arg(long)]
        limit: Option<usize>,

        #[arg(long, default_value_t = 4)]
        concurrency: usize,
    },

    /// Ask only the cheap first model what documents are: receipt or not,
    /// paid or due, seats, usage, card. Exits 2 on errors.
    Decide {
        #[arg(required = true)]
        inputs: Vec<PathBuf>,

        /// jev, clef, clef-flash or systemone, optionally with /model.
        #[arg(long, short)]
        decider: Option<String>,

        /// One JSON object per line.
        #[arg(long)]
        json: bool,

        #[arg(long, default_value_t = 8)]
        concurrency: usize,
    },
}

#[derive(Subcommand)]
enum CloudAction {
    /// Sign this machine in: a code to approve in the browser, for one of
    /// your organisations. The token goes to the OS keychain.
    Login {
        /// The server. Default: SPENDYARD_CLOUD_URL, then
        /// https://app.spendyard.com.
        #[arg(long)]
        server: Option<String>,

        /// Only print the link: don't open it in the browser.
        #[arg(long)]
        no_browser: bool,
    },

    /// Who this machine is signed in as, and the organisation it acts in.
    /// Exits 1 when it isn't signed in.
    Status,

    /// Act in another of your organisations. Without a slug, shows the one
    /// this machine acts in and the others.
    Switch {
        /// The organisation's slug, as the list shows it.
        slug: Option<String>,
    },

    /// Sign this machine out, here and on the server.
    Logout,
}

#[derive(Subcommand)]
enum MailboxAction {
    /// Sign in to a mailbox. Outlook signs in with a code you enter in the
    /// browser; the token goes to the OS keychain.
    Login {
        /// outlook or imap (Gmail, iCloud, Fastmail, Yahoo and others, with
        /// an app password)
        provider: String,

        /// The mailbox address, for imap.
        address: Option<String>,

        /// The IMAP server as host:port. Default: guessed from the address.
        #[arg(long)]
        server: Option<String>,

        /// The IMAP user name, when it isn't the address.
        #[arg(long)]
        username: Option<String>,

        /// The OAuth app to sign in with. Default: the config.
        #[arg(long)]
        client_id: Option<String>,

        /// Microsoft tenant ID or domain. Default: the config, then
        /// `organizations`.
        #[arg(long)]
        tenant: Option<String>,
    },

    /// List signed-in mailboxes.
    List,

    /// Read receipts straight from a mailbox: list messages since the last
    /// scan, keep the ones likely about money, and read those.
    Scan {
        /// outlook or imap
        provider: String,

        /// Which signed-in account, when there are several.
        #[arg(long)]
        account: Option<String>,

        /// A folder name such as Inbox or Receipts. Default: all folders.
        #[arg(long)]
        folder: Option<String>,

        /// YYYY-MM-DD. Default: where the last scan stopped, or 30 days ago.
        #[arg(long)]
        since: Option<String>,

        /// Look at most at this many messages, newest first.
        #[arg(long, default_value_t = 500)]
        limit: usize,

        /// Read every message, not only the ones that look like money.
        #[arg(long)]
        all: bool,

        /// List what would be read, without downloading or calling models.
        #[arg(long)]
        dry_run: bool,

        /// Also save every message read as .eml in this folder.
        #[arg(long)]
        save: Option<PathBuf>,

        /// Read messages again even when the store already has them.
        #[arg(long)]
        again: bool,

        /// Don't use the store: no saved position, nothing kept.
        #[arg(long)]
        no_store: bool,

        #[arg(long, short)]
        extractor: Option<String>,

        #[arg(long, short)]
        decider: Option<String>,

        /// Print readable summaries instead of JSON lines.
        #[arg(long)]
        pretty: bool,

        #[arg(long, default_value_t = 4)]
        concurrency: usize,
    },

    /// Sign out of a mailbox and delete its token.
    Logout {
        /// The address, or provider:address.
        account: String,
    },
}

#[derive(Subcommand)]
enum TransactionsAction {
    /// List them, newest last, with the receipt each one matched.
    List {
        /// Only purchases with no receipt.
        #[arg(long)]
        missing: bool,

        /// Only this card, by its last four digits.
        #[arg(long)]
        card: Option<String>,

        /// Only this month, as YYYY-MM.
        #[arg(long)]
        month: Option<String>,

        #[arg(long)]
        json: bool,
    },

    /// Match every payment on a statement to receipts again, after new receipts came in.
    Match {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum VendorsAction {
    /// Find a vendor by name, domain or email address.
    Lookup { query: String },

    /// List vendors, optionally of one category.
    List {
        #[arg(long)]
        category: Option<String>,
    },

    /// Vendors on receipts in your store that the catalog lacks.
    Unknown,

    /// Add a vendor the catalog lacks, as a local file in
    /// ~/.spendyard/vendors/: `spendyard vendors add fjordbyra.no --name Fjordbyrå`.
    Add {
        /// Its main domain, which becomes its ID.
        domain: String,

        /// The name to show.
        #[arg(long)]
        name: String,

        /// ai, dev-tools, infrastructure, observability, security, data,
        /// design, productivity, communication, analytics, marketing, sales,
        /// support, finance, hr, legal, education or other.
        #[arg(long, default_value = "other")]
        category: String,

        /// Another name on its receipts: a legal or old name. Repeat for more.
        #[arg(long = "alias")]
        aliases: Vec<String>,

        /// Another domain its site or mail uses. Repeat for more.
        #[arg(long = "also-domain")]
        domains: Vec<String>,

        /// How its charges start on card statements. Repeat for more.
        #[arg(long = "descriptor")]
        descriptors: Vec<String>,

        /// seat, usage, plan or one_time. Repeat for more.
        #[arg(long)]
        pricing: Vec<String>,

        /// Replace a local file that's already there.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Write a commented starter config to ~/.spendyard/config.toml.
    Init,
}

fn main() -> ExitCode {
    // Before the async runtime starts any threads: this sets variables.
    let env_files = cli::env_files::load();

    // Local vendor files join the catalog before anything looks one up.
    cli::vendors::load_local();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("an async runtime");

    runtime.block_on(run(env_files))
}

async fn run(env_files: Vec<PathBuf>) -> ExitCode {
    let cli = parse_arguments();

    let result = match cli.command {
        Command::Read {
            inputs,
            extractor,
            decider,
            pretty,
            jsonl,
            concurrency,
            no_store,
        } => {
            let options = cli::read::Options {
                inputs,
                extractor,
                decider,
                pretty,
                jsonl,
                concurrency,
                no_store,
            };

            cli::read::run(options).await.map(|outcome| {
                if outcome.errors > 0 {
                    ExitCode::from(2)
                } else if outcome.found > 0 {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::from(1)
                }
            })
        }

        Command::Eval { action } => eval(action).await,

        Command::Parts { input, json, text } => {
            cli::parts::run(&input, json, text).map(|()| ExitCode::SUCCESS)
        }

        Command::Cloud { action } => cloud(action).await,

        Command::Mailbox { action } => mailbox(action).await,

        Command::Documents { action } => documents(action).await,

        Command::Report {
            by,
            months,
            currency,
            json,
        } => cli::reports::report(cli::reports::ReportOptions {
            by,
            months,
            currency,
            json,
        })
        .await
        .map(|()| ExitCode::SUCCESS),

        Command::Subscriptions {
            currency,
            json,
            all,
        } => cli::reports::subscriptions(cli::reports::SubscriptionsOptions {
            currency,
            json,
            all,
        })
        .await
        .map(|()| ExitCode::SUCCESS),

        Command::Mcp { extractor, decider } => {
            let mcp = async {
                let config = cli::config::load()?;

                let models =
                    cli::models::Models::load(&config, extractor.as_deref(), decider.as_deref())?;

                cli::mcp::run(models).await
            };

            mcp.await.map(|()| ExitCode::SUCCESS)
        }

        Command::Payments {
            missing,
            vendor,
            limit,
            json,
        } => cli::payments::list(cli::payments::PaymentsOptions {
            missing,
            vendor,
            limit,
            json,
        })
        .await
        .map(|()| ExitCode::SUCCESS),

        Command::Transactions { action } => {
            let done = async {
                match action {
                    Some(TransactionsAction::List {
                        missing,
                        card,
                        month,
                        json,
                    }) => cli::charges::list(cli::charges::ListOptions {
                        missing,
                        card,
                        month,
                        json,
                    }),

                    Some(TransactionsAction::Match { json }) => {
                        cli::charges::match_command(json).await
                    }

                    None => cli::charges::match_command(false).await,
                }
            };

            done.await.map(|()| ExitCode::SUCCESS)
        }

        Command::Vendors { action } => {
            let done = match action {
                None => {
                    cli::vendors::summary();

                    Ok(())
                }

                Some(VendorsAction::Lookup { query }) => {
                    cli::vendors::lookup(&query);

                    Ok(())
                }

                Some(VendorsAction::List { category }) => {
                    cli::vendors::list(category.as_deref());

                    Ok(())
                }

                Some(VendorsAction::Unknown) => cli::vendors::unknown(),

                Some(VendorsAction::Add {
                    domain,
                    name,
                    category,
                    aliases,
                    domains,
                    descriptors,
                    pricing,
                    force,
                }) => cli::vendors::add(
                    spendyard_core::catalog::Vendor {
                        id: domain,
                        name,
                        aliases,
                        domains,
                        descriptors,
                        related: Vec::new(),
                        category,
                        pricing,
                        local: true,
                    },
                    force,
                ),
            };

            done.map(|()| ExitCode::SUCCESS)
        }

        Command::Config { action } => {
            let done = match action {
                Some(ConfigAction::Init) => cli::config_command::init(),
                None => cli::config_command::show(&env_files),
            };

            done.map(|()| ExitCode::SUCCESS)
        }
    };

    match result {
        Ok(code) => code,

        Err(error) => {
            eprintln!("spendyard: {error:#}");

            ExitCode::from(2)
        }
    }
}

/// `spendyard documents …`: what the store holds, and corrections to it.
async fn documents(action: DocumentsAction) -> anyhow::Result<ExitCode> {
    match action {
        DocumentsAction::Ui => cli::documents_ui::run().await.map(|()| ExitCode::SUCCESS),

        DocumentsAction::Get { id, json } => {
            cli::documents::get(cli::documents::GetOptions { id, json })
                .await
                .map(|()| ExitCode::SUCCESS)
        }

        DocumentsAction::List {
            vendor,
            since,
            limit,
            all,
            json,
        } => cli::reports::receipts(cli::reports::ReceiptsOptions {
            vendor,
            since,
            limit,
            all,
            json,
        })
        .map(|()| ExitCode::SUCCESS),

        DocumentsAction::Edit {
            id,
            personal,
            ignore,
            restore,
            vendor,
            note,
        } => {
            let flag = |set: bool| {
                if restore {
                    Some(false)
                } else {
                    set.then_some(true)
                }
            };

            cli::reports::edit(cli::reports::EditOptions {
                id,
                personal: flag(personal),
                ignore: flag(ignore),
                vendor,
                note,
            })
            .map(|()| ExitCode::SUCCESS)
        }
    }
}

/// `spendyard eval …`: expected answers for a folder, and scoring against them.
async fn eval(action: EvalAction) -> anyhow::Result<ExitCode> {
    match action {
        EvalAction::Run {
            dir,
            decider,
            extractor,
            limit,
            concurrency,
            wrong,
            json,
        } => {
            let options = cli::eval::EvalOptions {
                dir,
                deciders: decider,
                extractors: extractor,
                limit,
                concurrency,
                wrong,
                json,
            };

            cli::eval::run(options).await.map(|()| ExitCode::SUCCESS)
        }

        EvalAction::Label {
            dir,
            extractor,
            limit,
            concurrency,
        } => {
            let options = cli::eval::LabelOptions {
                dir,
                extractor,
                limit,
                concurrency,
            };

            cli::eval::label(options).await.map(|()| ExitCode::SUCCESS)
        }

        EvalAction::Decide {
            inputs,
            decider,
            json,
            concurrency,
        } => {
            let options = cli::decide::Options {
                inputs,
                decider,
                json,
                concurrency,
            };

            cli::decide::run(options).await.map(|errors| {
                if errors > 0 {
                    ExitCode::from(2)
                } else {
                    ExitCode::SUCCESS
                }
            })
        }
    }
}

/// `spendyard cloud …`: this machine's sign-in to Spendyard Cloud.
async fn cloud(action: CloudAction) -> anyhow::Result<ExitCode> {
    match action {
        CloudAction::Login { server, no_browser } => cli::cloud::login(server, !no_browser)
            .await
            .map(|()| ExitCode::SUCCESS),

        CloudAction::Status => cli::cloud::status().await.map(|signed_in| {
            if signed_in {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }),

        CloudAction::Switch { slug } => cli::cloud::switch(slug.as_deref())
            .await
            .map(|()| ExitCode::SUCCESS),

        CloudAction::Logout => cli::cloud::logout().await.map(|()| ExitCode::SUCCESS),
    }
}

/// `spendyard mailbox …`: sign-ins and scans of the mailboxes the CLI reads.
async fn mailbox(action: MailboxAction) -> anyhow::Result<ExitCode> {
    match action {
        MailboxAction::Login {
            provider,
            address,
            server,
            username,
            client_id,
            tenant,
        } => cli::mailbox_commands::login(cli::mailbox_commands::LoginOptions {
            provider,
            address,
            client_id,
            tenant,
            server,
            username,
        })
        .await
        .map(|()| ExitCode::SUCCESS),

        MailboxAction::Logout { account } => {
            cli::mailbox_commands::logout(&account).map(|()| ExitCode::SUCCESS)
        }

        MailboxAction::List => cli::mailbox_commands::list_accounts().map(|()| ExitCode::SUCCESS),

        MailboxAction::Scan {
            provider,
            account,
            folder,
            since,
            limit,
            all,
            dry_run,
            save,
            again,
            no_store,
            extractor,
            decider,
            pretty,
            concurrency,
        } => {
            let command = cli::mailbox_commands::ScanCommand {
                provider,
                options: cli::mailbox::ScanOptions {
                    account,
                    folder,
                    since,
                    limit,
                    all,
                    dry_run,
                    save,
                    again,
                    concurrency,
                },
                extractor,
                decider,
                pretty,
                no_store,
            };

            cli::mailbox_commands::scan(command)
                .await
                .map(|(found, errors)| {
                    if errors > 0 {
                        ExitCode::from(2)
                    } else if found > 0 || dry_run {
                        ExitCode::SUCCESS
                    } else {
                        ExitCode::from(1)
                    }
                })
        }
    }
}

/// The command line, with `--version` also giving the store's schema.
fn parse_arguments() -> Cli {
    let version = format!(
        "{}\nschema {}",
        spendyard_core::VERSION,
        spendyard_core::SCHEMA_VERSION
    );

    // clap keeps a version for the life of the program.
    let version: &'static str = Box::leak(version.into_boxed_str());

    let matches = Cli::command().version(version).get_matches();

    Cli::from_arg_matches(&matches).unwrap_or_else(|error| error.exit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_is_valid() {
        Cli::command().debug_assert();
    }
}
