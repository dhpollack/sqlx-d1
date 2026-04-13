mod input;
mod output;
mod sqlite_files;
mod wrangler_config;

use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use serde_json;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use syn::LitStr;

use self::wrangler_config::{extract_database_ids, find_wrangler_config, parse_wrangler_config};
use self::sqlite_files::expected_sqlite_filename;

struct Location {
    manifest_dir: PathBuf,
    workspace_root: LazyLock<PathBuf>,
}
/// ref: <https://github.com/launchbadge/sqlx/blob/1c7b3d0751cdca5a08fbfa7f24c985fc3774cf11/sqlx-macros-core/src/query/mod.rs#L80-L114>
static LOCATION: LazyLock<Location> = LazyLock::new(|| {
    fn get_manifest_dir() -> PathBuf {
        std::env::var("CARGO_MANIFEST_DIR")
            .expect("`CARGO_MANIFEST_DIR` must be set")
            .into()
    }

    fn get_workspace_root() -> PathBuf {
        use serde::Deserialize;
        use std::process::Command;

        let cargo = std::env::var("CARGO").expect("`CARGO` must be set");

        let output = Command::new(&cargo)
            .args(["metadata", "--format-version=1", "--no-deps"])
            .current_dir(get_manifest_dir())
            .env_remove("__CARGO_FIX_PLZ")
            .output()
            .expect("Could not fetch metadata");

        #[derive(Deserialize)]
        struct CargoMetadata {
            workspace_root: PathBuf,
        }

        let cargo_metadata: CargoMetadata =
            serde_json::from_slice(&output.stdout).expect("Invalid `cargo metadata` output");

        cargo_metadata.workspace_root
    }

    Location {
        manifest_dir: get_manifest_dir(),
        workspace_root: LazyLock::new(get_workspace_root),
    }
});
impl Location {
    fn miniflare_sqlite_files(&self) -> Result<Vec<PathBuf>, io::Error> {
        fn miniflare_d1_dir_path_in_parent(parent_path: impl AsRef<Path>) -> PathBuf {
            parent_path
                .as_ref()
                .join(".wrangler")
                .join("state")
                .join("v3")
                .join("d1")
                .join("miniflare-D1DatabaseObject")
        }

        let miniflare_d1_dir = 'search: {
            for parent_candidate in [&*LOCATION.manifest_dir, &*LOCATION.workspace_root] {
                let candidate = miniflare_d1_dir_path_in_parent(parent_candidate);
                if std::fs::exists(&candidate)? && candidate.is_dir() {
                    break 'search candidate;
                }
            }
            return Ok(Vec::new());
        };

        let sqlite_files = std::fs::read_dir(miniflare_d1_dir)?
            .filter_map(|r| r.as_ref().ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "sqlite"))
            .collect::<Vec<_>>();

        match sqlite_files.len() {
            0 => Ok(Vec::new()),
            1 => Ok(vec![sqlite_files.into_iter().next().unwrap()]),
            _ => {
                // Multiple SQLite files - need to use wrangler config to identify the correct one

                // Try to find wrangler config in manifest dir, then workspace root
                let config_path = find_wrangler_config(&self.manifest_dir)
                    .or_else(|_| find_wrangler_config(&self.workspace_root))?
                    .ok_or_else(|| io::Error::other(
                        "Multiple SQLite files found but no wrangler config (wrangler.jsonc or wrangler.toml) found"
                    ))?;

                let config = parse_wrangler_config(&config_path)?;
                let database_ids = extract_database_ids(&config);

                if database_ids.is_empty() {
                    return Err(io::Error::other(
                        "Multiple SQLite files found but wrangler config has no d1_databases"
                    ));
                }

                // Compute expected filenames for each database ID
                let expected_filenames: Vec<String> = database_ids
                    .iter()
                    .map(|id| expected_sqlite_filename(id))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| io::Error::other(format!("Failed to compute SQLite filename: {}", e)))?;

                // Match SQLite files
                let matches: Vec<PathBuf> = sqlite_files
                    .into_iter()
                    .filter(|path| {
                        let stem = path.file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("");
                        expected_filenames.contains(&stem.to_string())
                    })
                    .collect();

                if matches.is_empty() {
                    Err(io::Error::other(
                        "Multiple SQLite files found but none match database IDs in wrangler config"
                    ))
                } else {
                    Ok(matches)
                }
            }
        }
    }

    fn dot_sqlx_dir(&self) -> Result<Option<DotSqlx>, io::Error> {
        for parent_candidate in [&*LOCATION.manifest_dir, &*LOCATION.workspace_root] {
            if let Some(it) = DotSqlx::find_in_parent(parent_candidate)? {
                return Ok(Some(it));
            }
        }
        Ok(None)
    }
}

struct DotSqlx(PathBuf);
impl DotSqlx {
    fn find_in_parent(parent_dir: &Path) -> Result<Option<Self>, io::Error> {
        let candidate = parent_dir.join(".sqlx");
        if std::fs::exists(&candidate)? && candidate.is_dir() {
            Ok(Some(DotSqlx(candidate)))
        } else {
            Ok(None)
        }
    }

    fn file_path_of(&self, sql: &str) -> PathBuf {
        /* ref: <https://github.com/launchbadge/sqlx/blob/6651d2df72586519708147d96e1ec1054a898c1e/sqlx-macros-core/src/query/data.rs#L193-L198> */
        let hash = {
            use sha2::{Digest, Sha256};
            ::hex::encode(Sha256::digest(sql.as_bytes()))
        };

        /* ref:
            <https://github.com/launchbadge/sqlx/blob/6651d2df72586519708147d96e1ec1054a898c1e/sqlx-macros-core/src/query/mod.rs#L165>
            <https://github.com/launchbadge/sqlx/blob/6651d2df72586519708147d96e1ec1054a898c1e/sqlx-macros-core/src/query/data.rs#L156>
        */
        let file_name = format!("query-{hash}.json");

        self.0.join(file_name)
    }

    fn get_cached_describe_of(
        &self,
        sql: &str,
    ) -> Result<Option<sqlx_core::describe::Describe<sqlx_d1_core::D1>>, io::Error> {
        match std::fs::read(self.file_path_of(sql)) {
            Ok(bytes) => {
                let describe = ::serde_json::from_slice(&bytes).map_err(|e| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("failed to parse the query cache of `{sql}`: {e}"),
                    )
                })?;
                Ok(Some(describe))
            }
            Err(e) => {
                if matches!(e.kind(), io::ErrorKind::NotFound) {
                    Ok(None)
                } else {
                    Err(e)
                }
            }
        }
    }

    /// ref: <https://github.com/launchbadge/sqlx/blob/6651d2df72586519708147d96e1ec1054a898c1e/sqlx-macros-core/src/query/data.rs#L153-L190>
    fn cache_describe(
        &self,
        sql: &str,
        describe: sqlx_core::describe::Describe<sqlx_d1_core::D1>,
    ) -> Result<(), io::Error> {
        let describe = ::serde_json::to_vec(&describe).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("failed to serialize the query cache of `{sql}`: {e}"),
            )
        })?;
        std::fs::write(self.file_path_of(sql), describe)?;
        Ok(())
    }
}

pub(super) fn expand_input(input: TokenStream) -> Result<TokenStream, syn::Error> {
    // Catch panics to avoid "crashed background worker" errors
    let result = std::panic::catch_unwind(|| -> Result<TokenStream, syn::Error> {
        use sqlx_core::executor::Executor;
        use sqlx_d1_core::D1Connection;
        // Assumeing the cost of context switching is almost the same
        // or larger than that of synchronous blocking in this case

        let input = syn::parse2::<self::input::QueryMacroInput>(input)?;

        let describe = match LOCATION.miniflare_sqlite_files().map_err(|e| syn::Error::new(Span::call_site(), e))? {
            sqlite_file_paths if !sqlite_file_paths.is_empty() => {
                // Collect describes from all databases
                let mut describes = Vec::new();
                for sqlite_file_path in sqlite_file_paths {
                    // Create connection for this file
                    let conn = futures_lite::future::block_on(async {
                        D1Connection::connect(&format!("sqlite://{}", sqlite_file_path.display()))
                            .await
                            .map_err(|e| syn::Error::new(Span::call_site(), e))
                    })?;

                    let describe = futures_lite::future::block_on(async {
                        conn.describe(&input.sql).await
                    }).map_err(|e| syn::Error::new(input.src_span, e))?;

                    describes.push((sqlite_file_path, describe));
                }

                // Ensure all describes are identical
                // Take first describe out of the vector to own it
                let (first_path, first_describe) = describes.remove(0);
                for (path, describe) in &describes {
                    // Compare describes by serializing to JSON
                    let first_json = serde_json::to_string(&first_describe)
                        .map_err(|e| syn::Error::new(Span::call_site(), format!("Failed to serialize describe: {}", e)))?;
                    let current_json = serde_json::to_string(describe)
                        .map_err(|e| syn::Error::new(Span::call_site(), format!("Failed to serialize describe: {}", e)))?;

                    if first_json != current_json {
                        return Err(syn::Error::new(
                            Span::call_site(),
                            format!("Schema mismatch between databases. Query validation differs between databases. First database: {:?}, current database: {:?}",
                                    first_path, path)
                        ));
                    }
                }

                // All describes match, use first one
                first_describe
            }

            // No SQLite files found, fall back to .sqlx cache
            _ => match LOCATION.dot_sqlx_dir().map_err(|e| syn::Error::new(input.src_span, e))? {
                Some(dot_sqlx_dir) => dot_sqlx_dir
                    .get_cached_describe_of(&input.sql)
                    .map_err(|e| syn::Error::new(input.src_span, e))?
                    .ok_or_else(|| syn::Error::new(
                        input.src_span,
                        "there is no cached data for this query, run `cargo sqlx prepare` to update the query cache"
                    ))?,

                None => return Err(syn::Error::new(
                    input.src_span,
                    "Neither miniflare D1 emulator nor .sqlx directory is found ! \n\
                    For setting up miniflare, run \
                    `wrangler d1 migrations create <BINDING> <MIGRATION>` and \
                    `wrangler d1 migrations apply <BINDING> --local`.\n\
                    For setting up .sqlx directory for offline mode, \
                    run `cargo sqlx prepare` where `cargo sqlx` is installed and \
                    miniflare D1 emulator is accessable (offen your local PC)."
                ))
            }
        };

        compare_expand(input, describe)
    });

    match result {
        Ok(inner_result) => inner_result,
        Err(panic) => {
            // Convert panic to a user-friendly error
            let panic_msg = if let Some(s) = panic.downcast_ref::<String>() {
                s.clone()
            } else if let Some(s) = panic.downcast_ref::<&str>() {
                s.to_string()
            } else {
                "Unknown panic".to_string()
            };
            Err(syn::Error::new(
                Span::call_site(),
                format!("Macro panicked: {}", panic_msg)
            ))
        }
    }
}

/// ref: <https://github.com/launchbadge/sqlx/blob/1c7b3d0751cdca5a08fbfa7f24c985fc3774cf11/sqlx-macros-core/src/query/mod.rs#L241-379>
fn compare_expand(
    input: self::input::QueryMacroInput,
    describe: sqlx_core::describe::Describe<sqlx_d1_core::D1>,
) -> Result<TokenStream, syn::Error> {
    if let Some(actual_params) = describe.parameters() {
        use sqlx_core::Either;

        let n_input_params = input.arg_exprs.len();
        let n_correct_params = match actual_params {
            Either::Left(params) => params.len(),
            Either::Right(n) => n,
        };

        let _: () = (n_input_params == n_correct_params)
            .then_some(())
            .ok_or_else(|| {
                syn::Error::new(
                    Span::call_site(),
                    format!("expected {n_correct_params} parameters, got {n_input_params}"),
                )
            })?;
    }

    let args_tokens = input.quote_args_with(&describe)?;

    let query_args_ident = format_ident!("query_args");

    let output = if describe.columns().iter().all({
        use sqlx_core::{column::Column as _, type_info::TypeInfo as _};
        |c| c.type_info().is_void()
    }) {
        let sql = LitStr::new(&input.sql, input.src_span);
        quote! {
            ::sqlx_d1::sqlx_core::query::query_with_result::<::sqlx_d1::D1, _>(#sql, #query_args_ident)
        }
    } else {
        match &input.record_type {
            input::RecordType::Scalar => {
                output::quote_query_scalar(&input, &query_args_ident, &describe)?
            }
            input::RecordType::Given(out_ty) => {
                let columns = output::columns_to_rust(&describe)?;
                output::quote_query_as(&input, out_ty, &query_args_ident, &columns)
            }
            input::RecordType::Generated => {
                let columns = self::output::columns_to_rust(&describe)?;

                let record_type_name_token = syn::parse_str::<syn::Type>("Record")
                    .expect("Failed to parse 'Record' as Type - this should never happen");

                for rust_column in &columns {
                    if rust_column.type_.is_wildcard() {
                        return Err(syn::Error::new(
                            rust_column.ident.span(),
                            "wildcard overrides are only allowed with an explicit record type, \
                            e.g. `query_as!()` and its variants",
                        ));
                    }
                }

                let record_fields = columns.iter().map(|rc| {
                    let (ident, type_) = (&rc.ident, &rc.type_);
                    quote! {
                        #ident: #type_,
                    }
                });

                let mut record_tokens = quote! {
                    #[derive(Debug)]
                    struct #record_type_name_token {
                        #(#record_fields)*
                    }
                };
                record_tokens.extend(output::quote_query_as(
                    &input,
                    &record_type_name_token,
                    &query_args_ident,
                    &columns,
                ));

                record_tokens
            }
        }
    };

    if let Some(dot_sqlx_dir) = LOCATION
        .dot_sqlx_dir()
        .map_err(|e| syn::Error::new(input.src_span, e))?
    {
        dot_sqlx_dir
            .cache_describe(&input.sql, describe)
            .map_err(|e| syn::Error::new(input.src_span, e))?;
    }

    Ok(quote! {
        {
            #[allow(clippy::all)]
            {
                use ::sqlx_d1::sqlx_core::arguments::Arguments as _;

                #args_tokens

                #output
            }
        }
    })
}
