//! `spendyard api …`: Spendyard Cloud's API by its contract
//! (`proto/spendyard/api/v1`), for what no command covers yet, for scripts
//! and for coding agents: its methods listed with the permission each takes,
//! one described field by field, and any of them called with a request
//! written as JSON, as this machine, in the organisation it acts in.
//!
//! The contract is compiled in (`contract.binpb`, built by
//! `scripts/api-contract.sh` with `buf build`), so a request is checked
//! against its message before it's sent, and `list` and `describe` need no
//! cloud at all. A test fails when `proto/` changes and the build wasn't run.

use std::sync::OnceLock;

use anyhow::{Context, Result, bail};
use prost_reflect::{
    DescriptorPool, DeserializeOptions, DynamicMessage, FieldDescriptor, FileDescriptor, Kind,
    MessageDescriptor, MethodDescriptor, SerializeOptions,
};
use serde_json::Value;

use crate::cli::cloud::Cloud;

/// The contract's descriptors, comments and all, with what it imports.
static CONTRACT: &[u8] = include_bytes!("contract.binpb");

/// The package the API is: `spendyard.api.v1`.
const PACKAGE: &str = "spendyard.api.v1";

fn pool() -> &'static DescriptorPool {
    static POOL: OnceLock<DescriptorPool> = OnceLock::new();

    POOL.get_or_init(|| DescriptorPool::decode(CONTRACT).expect("the compiled-in contract"))
}

pub struct ListOptions {
    /// Only those whose `Service/Method` holds this, whatever its case.
    pub text: Option<String>,
    /// Only those of a service whose name holds this.
    pub service: Option<String>,
    /// Only those taking a `:read` permission.
    pub reads: bool,
    /// Only those taking any other.
    pub writes: bool,
    /// Only those that answer with a stream.
    pub streams: bool,
}

// --- the contract --------------------------------------------------------------------------------

/// Every method of the API, service by service, as the contract orders them.
fn methods() -> Vec<MethodDescriptor> {
    pool()
        .services()
        .filter(|service| service.package_name() == PACKAGE)
        .flat_map(|service| service.methods().collect::<Vec<_>>())
        .collect()
}

/// `DocumentService/ListDocuments`: how commands and the API's URL name a method.
fn path_of(method: &MethodDescriptor) -> String {
    format!("{}/{}", method.parent_service().name(), method.name())
}

/// A string option of the contract's own on a method (`permission`), or a
/// boolean one (`interactive_only`) as "true".
fn option_of(method: &MethodDescriptor, name: &str) -> Option<String> {
    let extension = pool().get_extension_by_name(&format!("{PACKAGE}.{name}"))?;
    let options = method.options();

    if !options.has_extension(&extension) {
        return None;
    }

    let value = options.get_extension(&extension);

    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.as_bool().filter(|on| *on).map(|_| "true".to_string()))
}

fn permission_of(method: &MethodDescriptor) -> String {
    option_of(method, "permission").unwrap_or_default()
}

fn interactive_only(method: &MethodDescriptor) -> bool {
    option_of(method, "interactive_only").is_some()
}

/// The comment written above something in its `.proto` file, by its path
/// there: the contract's own words for it.
fn comment(file: &FileDescriptor, path: &[i32]) -> Option<String> {
    let info = file.file_descriptor_proto().source_code_info.as_ref()?;
    let location = info
        .location
        .iter()
        .find(|location| location.path == path)?;
    let text = location.leading_comments.as_deref()?.trim();

    let lines: Vec<&str> = text.lines().map(str::trim).collect();

    (!text.is_empty()).then(|| lines.join(" "))
}

/// The method a name means: `DocumentService/ListDocuments`,
/// `DocumentService.ListDocuments`, `spendyard.api.v1.DocumentService/ListDocuments`,
/// or `ListDocuments` when only one service has it; whatever its case.
fn find_method(name: &str) -> Result<MethodDescriptor> {
    let wanted = name
        .trim()
        .trim_start_matches('/')
        .trim_start_matches(&format!("{PACKAGE}."))
        .replace('.', "/")
        .to_lowercase();

    let all = methods();

    let by_path: Vec<&MethodDescriptor> = all
        .iter()
        .filter(|method| path_of(method).to_lowercase() == wanted)
        .collect();

    if let [method] = by_path.as_slice() {
        return Ok((*method).clone());
    }

    let by_name: Vec<&MethodDescriptor> = all
        .iter()
        .filter(|method| method.name().to_lowercase() == wanted)
        .collect();

    match by_name.as_slice() {
        [method] => Ok((*method).clone()),
        [] => bail!("no method {name}; `spendyard api list` shows them all"),
        several => {
            let paths: Vec<String> = several.iter().map(|method| path_of(method)).collect();

            bail!("{name} is several methods: {}", paths.join(", "))
        }
    }
}

// --- list ----------------------------------------------------------------------------------------

/// `spendyard api list`: the methods, with the permission each takes.
pub fn list(options: ListOptions) -> Result<()> {
    if options.reads && options.writes {
        bail!("--reads or --writes, not both");
    }

    let text = options.text.map(|text| text.to_lowercase());
    let service = options.service.map(|service| service.to_lowercase());

    let picked: Vec<MethodDescriptor> = methods()
        .into_iter()
        .filter(|method| {
            let path = path_of(method).to_lowercase();
            let permission = permission_of(method);

            let by_text = text.as_ref().is_none_or(|text| path.contains(text));

            let by_service = service.as_ref().is_none_or(|service| {
                method
                    .parent_service()
                    .name()
                    .to_lowercase()
                    .contains(service)
            });

            let reads = permission.ends_with(":read");

            by_text
                && by_service
                && (!options.reads || reads)
                && (!options.writes || !reads)
                && (!options.streams || method.is_server_streaming())
        })
        .collect();

    if picked.is_empty() {
        println!("No methods pick; `spendyard api list` shows them all");

        return Ok(());
    }

    let widest = picked
        .iter()
        .map(|method| path_of(method).len())
        .max()
        .unwrap_or(0);

    for method in &picked {
        let mut marks = Vec::new();

        if method.is_server_streaming() {
            marks.push("stream");
        }

        if interactive_only(method) {
            marks.push("dashboard only");
        }

        let marks = if marks.is_empty() {
            String::new()
        } else {
            format!("  ({})", marks.join(", "))
        };

        println!(
            "{:<widest$}  {}{marks}",
            path_of(method),
            permission_of(method)
        );
    }

    Ok(())
}

// --- describe ------------------------------------------------------------------------------------

/// `spendyard api describe <method>`: what it does, the permission it takes,
/// and its request and answer field by field, with the messages they hold.
pub fn describe(name: &str) -> Result<()> {
    let method = find_method(name)?;
    let file = method.parent_file();

    println!("{}  {}", path_of(&method), permission_of(&method));

    if let Some(said) = comment(&file, method.path()) {
        println!("  {said}");
    }

    if method.is_server_streaming() {
        println!("  Answers with a stream: `spendyard api call` prints each message until Ctrl-C.");
    }

    if interactive_only(&method) {
        println!("  Only in the dashboard: this machine's token is refused.");
    }

    let mut shown: Vec<String> = Vec::new();
    let mut waiting: Vec<MessageDescriptor> = vec![method.input(), method.output()];

    while let Some(message) = waiting.first().cloned() {
        waiting.remove(0);

        if shown.contains(&message.full_name().to_string()) {
            continue;
        }

        shown.push(message.full_name().to_string());

        println!();

        for nested in describe_message(&message) {
            let ours = nested.package_name() == PACKAGE;

            if ours && !shown.contains(&nested.full_name().to_string()) {
                waiting.push(nested);
            }
        }
    }

    Ok(())
}

/// Prints a message's fields, and gives the messages they hold, to describe next.
fn describe_message(message: &MessageDescriptor) -> Vec<MessageDescriptor> {
    let file = message.parent_file();

    println!("{}", message.name());

    if let Some(said) = comment(&file, message.path()) {
        println!("  {said}");
    }

    let mut held = Vec::new();

    for field in message.fields() {
        let said = comment(&file, field.path())
            .map(|said| format!("  {said}"))
            .unwrap_or_default();

        println!("  {:<24} {:<22}{said}", field.json_name(), type_of(&field));

        if let Kind::Message(inner) = field.kind() {
            if field.is_map() {
                if let Kind::Message(value) = inner.map_entry_value_field().kind() {
                    held.push(value);
                }
            } else {
                held.push(inner);
            }
        }
    }

    held
}

/// A field's type as JSON writes it: `string`, `[Document]`, `{string: string}`, `DocumentType`.
fn type_of(field: &FieldDescriptor) -> String {
    let kind_name = |kind: Kind| match kind {
        Kind::Message(message) => match message.full_name() {
            "google.protobuf.Timestamp" => "timestamp".to_string(),
            "google.protobuf.Struct" => "object".to_string(),
            _ => message.name().to_string(),
        },
        Kind::Enum(enumeration) => enumeration.name().to_string(),
        Kind::String => "string".to_string(),
        Kind::Bool => "bool".to_string(),
        Kind::Bytes => "bytes".to_string(),
        Kind::Double | Kind::Float => "number".to_string(),
        Kind::Int64 | Kind::Uint64 | Kind::Sint64 | Kind::Fixed64 | Kind::Sfixed64 => {
            "int64 (a string)".to_string()
        }
        _ => "integer".to_string(),
    };

    if field.is_map() {
        let Kind::Message(entry) = field.kind() else {
            return "map".to_string();
        };

        let key = kind_name(entry.map_entry_key_field().kind());
        let value = kind_name(entry.map_entry_value_field().kind());

        return format!("{{{key}: {value}}}");
    }

    let named = kind_name(field.kind());

    if field.is_list() {
        format!("[{named}]")
    } else {
        named
    }
}

// --- call ----------------------------------------------------------------------------------------

/// `spendyard api call <method> [json | -]`: the method called with the
/// request, as this machine, in the organisation it acts in. The answer as
/// JSON; a stream's messages one a line, until it ends or Ctrl-C.
pub async fn call(name: &str, request: Option<String>) -> Result<()> {
    let method = find_method(name)?;

    if method.is_client_streaming() {
        bail!(
            "{} takes a stream of requests, which `spendyard api call` can't send",
            path_of(&method)
        );
    }

    let cloud = Cloud::connect()?;

    let written = match request.as_deref() {
        None => "{}".to_string(),
        Some("-") => std::io::read_to_string(std::io::stdin()).context("reading the request")?,
        Some(json) => json.to_string(),
    };

    let acting_in = cloud.signed_in_as().org.id.clone();
    let request = checked_request(&method, &written, &acting_in)?;
    let path = path_of(&method);

    if method.is_server_streaming() {
        return cloud
            .stream(&path, request, |message| {
                println!("{message}");

                Ok(())
            })
            .await;
    }

    let answer: Value = cloud.call(&path, request).await?;

    println!("{}", serde_json::to_string_pretty(&answer)?);

    Ok(())
}

/// The request written, checked against its message: no field it hasn't
/// got, each of the type it takes. Its `orgId`, when it has one and none was
/// given, is the organisation this machine acts in. As protobuf's JSON gives
/// it back.
fn checked_request(method: &MethodDescriptor, written: &str, acting_in: &str) -> Result<Value> {
    let input = method.input();

    let mut request: Value = serde_json::from_str(if written.trim().is_empty() {
        "{}"
    } else {
        written
    })
    .context("the request isn't JSON")?;

    let names_org = input.get_field_by_name("org_id").is_some();

    if let Value::Object(fields) = &mut request
        && names_org
        && !fields.contains_key("orgId")
        && !fields.contains_key("org_id")
    {
        fields.insert("orgId".to_string(), Value::String(acting_in.to_string()));
    }

    let written = request.to_string();
    let mut deserializer = serde_json::Deserializer::from_str(&written);

    let message = DynamicMessage::deserialize_with_options(
        input.clone(),
        &mut deserializer,
        &DeserializeOptions::new(),
    )
    .map_err(|error| {
        anyhow::anyhow!(
            "the request isn't a {}: {error}; `spendyard api describe {}` shows its fields",
            input.name(),
            path_of(method)
        )
    })?;

    let mut out = serde_json::Serializer::new(Vec::new());

    message.serialize_with_options(&mut out, &SerializeOptions::new())?;

    Ok(serde_json::from_slice(&out.into_inner())?)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use sha2::{Digest, Sha256};

    use super::*;

    /// The contract's files, in the order `scripts/api-contract.sh` reads
    /// them, as one digest: what `contract.sha256` says was compiled in.
    /// Line endings aside, so it's the same on every system.
    fn digest_of(proto: &Path) -> String {
        fn walk(dir: &Path, found: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();

                if path.is_dir() {
                    walk(&path, found);
                } else if path
                    .extension()
                    .is_some_and(|extension| extension == "proto")
                {
                    found.push(path);
                }
            }
        }

        let mut files = Vec::new();

        walk(proto, &mut files);
        files.sort();

        let mut hasher = Sha256::new();

        // Without carriage returns: Git on Windows checks the files out with them.
        for file in files {
            let text: Vec<u8> = std::fs::read(file)
                .unwrap()
                .into_iter()
                .filter(|byte| *byte != b'\r')
                .collect();

            hasher.update(text);
        }

        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    #[test]
    fn the_compiled_in_contract_is_the_one_in_proto() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));

        let compiled = std::fs::read_to_string(root.join("src/cli/api/contract.sha256")).unwrap();

        assert_eq!(
            compiled.split_whitespace().next().unwrap(),
            digest_of(&root.join("proto")),
            "proto/ changed: run scripts/api-contract.sh"
        );
    }

    #[test]
    fn knows_every_method_with_its_permission() {
        let all = methods();

        assert!(all.len() > 30);
        assert!(all.iter().all(|method| !permission_of(method).is_empty()));

        let list = find_method("DocumentService/ListDocuments").unwrap();

        assert_eq!(permission_of(&list), "documents:read");
        assert!(
            comment(&list.parent_file(), list.path())
                .unwrap()
                .contains("newest first")
        );
    }

    #[test]
    fn finds_a_method_however_it_is_named() {
        for name in [
            "DocumentService/ListDocuments",
            "documentservice.listdocuments",
            "spendyard.api.v1.DocumentService/ListDocuments",
            "ListDocuments",
        ] {
            assert_eq!(
                path_of(&find_method(name).unwrap()),
                "DocumentService/ListDocuments"
            );
        }

        assert!(find_method("NoSuchThing").is_err());
        assert!(interactive_only(
            &find_method("OrgService/RemoveMember").unwrap()
        ));
    }

    #[test]
    fn checks_a_request_and_names_the_organisation() {
        let list = find_method("DocumentService/ListDocuments").unwrap();
        let push = find_method("PushService/PushDocuments").unwrap();

        let checked =
            checked_request(&list, r#"{"pageSize": 5, "query": "slack"}"#, "org-1").unwrap();
        let named = checked_request(&push, r#"{"documents": []}"#, "org-1").unwrap();
        let given = checked_request(&push, r#"{"orgId": "org-2"}"#, "org-1").unwrap();

        assert_eq!(checked["pageSize"], 5);
        assert_eq!(checked.get("orgId"), None);
        assert_eq!(named["orgId"], "org-1");
        assert_eq!(given["orgId"], "org-2");

        let unknown = checked_request(&list, r#"{"pageSiz": 5}"#, "org-1").unwrap_err();
        let wrong = checked_request(&list, r#"{"pageSize": "five"}"#, "org-1").unwrap_err();

        assert!(unknown.to_string().contains("isn't a ListDocumentsRequest"));
        assert!(wrong.to_string().contains("isn't a ListDocumentsRequest"));
    }

    #[test]
    fn writes_types_as_json_has_them() {
        let document = pool()
            .get_message_by_name("spendyard.api.v1.Document")
            .unwrap();
        let upload = pool()
            .get_message_by_name("spendyard.api.v1.CreateDocumentUploadResponse")
            .unwrap();

        let type_named = |message: &MessageDescriptor, name: &str| {
            type_of(&message.get_field_by_name(name).unwrap())
        };

        assert_eq!(type_named(&document, "vendor"), "VendorRef");
        assert_eq!(type_named(&document, "type"), "DocumentType");
        assert_eq!(type_named(&upload, "headers"), "{string: string}");
    }
}
