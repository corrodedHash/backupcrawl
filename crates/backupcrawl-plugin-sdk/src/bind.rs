//! The `bind_detector!` macro: wasm component export glue for a [`Detector`].
//!
//! [`Detector`]: crate::Detector

/// Emit the component export glue for a detector type.
///
/// `Type` must implement [`Detector`](crate::Detector) + [`Default`], and the
/// macro must be invoked in the same module as the type (usually the crate
/// root). The crate must contain a `wit` directory (or symlink) pointing at the
/// canonical `crates/backupcrawl-wit/wit` contract, because the macro calls
/// `wit_bindgen::generate!({ path: "wit", world: "plugin" })`; that path is
/// resolved relative to the crate's `CARGO_MANIFEST_DIR`.
///
/// Only the `backupcrawl-plugin-sdk` dependency is needed in the plugin crate:
/// the macro re-exports the pinned `wit-bindgen` entry points it uses.
///
/// The generated `check-files` implementation is exactly N singular
/// [`check_file`](crate::Detector::check_file) calls, as the frozen WIT `@1`
/// decision requires.
///
/// ```ignore
/// use backupcrawl_plugin_sdk::bind_detector;
///
/// #[derive(Default)]
/// struct Git;
///
/// impl backupcrawl_plugin_sdk::Detector for Git {
///     // ...
/// }
///
/// bind_detector!(Git);
/// ```
#[macro_export]
macro_rules! bind_detector {
    ($detector:ident) => {
        // Canonical component bindings. `runtime_path` points at the SDK's
        // re-export of the wit-bindgen runtime so a plugin crate only needs
        // this SDK as a dependency (see the crate docs on dependency naming).
        $crate::_wit_bindgen_generate!({
            path: "wit",
            world: "plugin",
            runtime_path: "backupcrawl_plugin_sdk::rt",
        });

        /// Generated wasm glue: a [`Host`](backupcrawl_plugin_sdk::Host)
        /// implementation over the imported host functions plus record
        /// conversions. Kept in its own module to avoid name collisions.
        #[doc(hidden)]
        #[allow(unused, clippy::all)]
        pub(crate) mod __backupcrawl_plugin_sdk_glue {
            use super::backupcrawl::detector::host as __host;
            use super::backupcrawl::detector::types as __wit;

            /// The real host, forwarding to the imported WIT functions.
            pub struct WasmHost;

            /// Convert a WIT file kind to the native one.
            pub fn file_kind_from_wit(kind: __host::FileKind) -> $crate::FileKind {
                match kind {
                    __host::FileKind::File => $crate::FileKind::File,
                    __host::FileKind::Directory => $crate::FileKind::Directory,
                    __host::FileKind::Symlink => $crate::FileKind::Symlink,
                    __host::FileKind::Other => $crate::FileKind::Other,
                }
            }

            /// Convert a WIT file info to the native one.
            pub fn file_info_from_wit(info: __host::FileInfo) -> $crate::FileInfo {
                $crate::FileInfo {
                    kind: file_kind_from_wit(info.kind),
                    size: info.size,
                    mtime_ms: info.mtime_ms,
                    mode: info.mode,
                    uid: info.uid,
                    gid: info.gid,
                    nlink: info.nlink,
                }
            }

            /// Convert a native exec request to the WIT one.
            pub fn exec_request_to_wit(request: &$crate::ExecRequest) -> __host::ExecRequest {
                __host::ExecRequest {
                    program: request.program.clone(),
                    args: request.args.clone(),
                    cwd: request
                        .cwd
                        .as_ref()
                        .map(|path| path.to_string_lossy().into_owned()),
                    env: request.env.clone(),
                    stdin: request.stdin.clone(),
                    timeout_ms: request.timeout_ms,
                }
            }

            /// Convert a WIT exec result to the native one.
            pub fn exec_result_from_wit(result: __host::ExecResult) -> $crate::ExecResult {
                $crate::ExecResult {
                    code: result.code,
                    stdout: result.stdout,
                    stderr: result.stderr,
                    timed_out: result.timed_out,
                }
            }

            /// Convert a native HTTP request to the WIT one.
            pub fn http_request_to_wit(request: &$crate::HttpRequest) -> __host::HttpRequest {
                __host::HttpRequest {
                    method: request.method.clone(),
                    url: request.url.clone(),
                    headers: request.headers.clone(),
                    body: request.body.clone(),
                }
            }

            /// Convert a WIT HTTP response to the native one.
            pub fn http_response_from_wit(response: __host::HttpResponse) -> $crate::HttpResponse {
                $crate::HttpResponse {
                    status: response.status,
                    headers: response.headers,
                    body: response.body,
                }
            }

            /// Convert a WIT status to the native one.
            pub fn status_from_wit(status: __wit::Status) -> $crate::Status {
                match status {
                    __wit::Status::Clean => $crate::Status::Clean,
                    __wit::Status::Dirty => $crate::Status::Dirty,
                    __wit::Status::Ahead => $crate::Status::Ahead,
                    __wit::Status::Unknown => $crate::Status::Unknown,
                    __wit::Status::Ignored => $crate::Status::Ignored,
                }
            }

            /// Convert a native status to the WIT one.
            pub fn status_to_wit(status: $crate::Status) -> __wit::Status {
                match status {
                    $crate::Status::Clean => __wit::Status::Clean,
                    $crate::Status::Dirty => __wit::Status::Dirty,
                    $crate::Status::Ahead => __wit::Status::Ahead,
                    $crate::Status::Unknown => __wit::Status::Unknown,
                    $crate::Status::Ignored => __wit::Status::Ignored,
                }
            }

            /// Convert a WIT attr to the native one.
            pub fn attr_from_wit(attr: __wit::Attr) -> $crate::Attr {
                $crate::Attr {
                    key: attr.key,
                    value: attr.value,
                }
            }

            /// Convert a native attr to the WIT one.
            pub fn attr_to_wit(attr: &$crate::Attr) -> __wit::Attr {
                __wit::Attr {
                    key: attr.key.clone(),
                    value: attr.value.clone(),
                }
            }

            /// Convert a WIT claim to the native one.
            pub fn claim_from_wit(claim: __wit::Claim) -> $crate::Claim {
                $crate::Claim {
                    status: status_from_wit(claim.status),
                    attrs: claim.attrs.into_iter().map(attr_from_wit).collect(),
                    prune: claim.prune,
                }
            }

            /// Convert a native claim to the WIT one.
            pub fn claim_to_wit(claim: $crate::Claim) -> __wit::Claim {
                __wit::Claim {
                    status: status_to_wit(claim.status),
                    attrs: claim.attrs.iter().map(attr_to_wit).collect(),
                    prune: claim.prune,
                }
            }

            /// Convert a WIT scan info to the native one.
            pub fn scan_info_from_wit(info: __wit::ScanInfo) -> $crate::ScanInfo {
                $crate::ScanInfo {
                    root: ::std::path::PathBuf::from(info.root),
                    user: info.user,
                    follow_symlinks: info.follow_symlinks,
                    config: $crate::Config::new(
                        info.config.into_iter().map(attr_from_wit).collect(),
                    ),
                }
            }

            /// Convert a WIT link info to the native one.
            pub fn link_info_from_wit(link: __wit::LinkInfo) -> $crate::LinkInfo {
                $crate::LinkInfo {
                    path: ::std::path::PathBuf::from(link.path),
                    target: ::std::path::PathBuf::from(link.target),
                    dangling: link.dangling,
                }
            }

            impl $crate::Host for WasmHost {
                fn stat(
                    &mut self,
                    path: &::std::path::Path,
                ) -> ::core::result::Result<$crate::FileInfo, $crate::HostError> {
                    let path = path.to_string_lossy();
                    __host::stat(path.as_ref())
                        .map(file_info_from_wit)
                        .map_err($crate::HostError::new)
                }

                fn exec(
                    &mut self,
                    request: &$crate::ExecRequest,
                ) -> ::core::result::Result<$crate::ExecResult, $crate::HostError> {
                    let request = exec_request_to_wit(request);
                    __host::exec(&request)
                        .map(exec_result_from_wit)
                        .map_err($crate::HostError::new)
                }

                fn cache_get(&mut self, key: &str) -> ::core::option::Option<::std::vec::Vec<u8>> {
                    __host::cache_get(key)
                }

                fn cache_set(
                    &mut self,
                    key: &str,
                    value: &[u8],
                ) -> ::core::result::Result<(), $crate::HostError> {
                    __host::cache_set(key, value).map_err($crate::HostError::new)
                }

                fn http(
                    &mut self,
                    request: &$crate::HttpRequest,
                ) -> ::core::result::Result<$crate::HttpResponse, $crate::HostError> {
                    let request = http_request_to_wit(request);
                    __host::http(&request)
                        .map(http_response_from_wit)
                        .map_err($crate::HostError::new)
                }

                fn log(&mut self, level: $crate::LogLevel, message: &str) {
                    __host::log(level.as_str(), message);
                }
            }
        }

        /// The detector instance for this component. The WIT export functions
        /// are associated functions without `self`, so state lives here; wasm
        /// is single-threaded and host calls into one instance are serialized.
        /// A parallel scan (M4a) builds one component instance per worker, each
        /// with its own thread-local state; see [`Detector`] for the per-worker
        /// contract.
        #[doc(hidden)]
        ::std::thread_local! {
            static __BACKUPCRAWL_DETECTOR: ::core::cell::RefCell<::core::option::Option<$detector>> =
                const { ::core::cell::RefCell::new(::core::option::Option::None) };
        }

        #[doc(hidden)]
        fn __backupcrawl_with_detector<R>(f: impl FnOnce(&mut $detector) -> R) -> R {
            __BACKUPCRAWL_DETECTOR.with(|cell| {
                let mut slot = cell.borrow_mut();
                let detector =
                    slot.get_or_insert_with(<$detector as ::core::default::Default>::default);
                f(detector)
            })
        }

        #[doc(hidden)]
        #[allow(unused, clippy::all)]
        impl self::exports::backupcrawl::detector::detector::Guest for $detector {
            fn init(
                info: self::backupcrawl::detector::types::ScanInfo,
            ) -> ::core::result::Result<(), ::std::string::String> {
                __backupcrawl_with_detector(|detector| {
                    let mut host = self::__backupcrawl_plugin_sdk_glue::WasmHost;
                    let info = self::__backupcrawl_plugin_sdk_glue::scan_info_from_wit(info);
                    <$detector as $crate::Detector>::init(detector, &info, &mut host)
                        .map_err(|error| error.to_string())
                })
            }

            fn check_dir(
                path: ::std::string::String,
            ) -> ::core::result::Result<
                ::core::option::Option<self::backupcrawl::detector::types::Claim>,
                ::std::string::String,
            > {
                __backupcrawl_with_detector(|detector| {
                    let mut host = self::__backupcrawl_plugin_sdk_glue::WasmHost;
                    let path = ::std::path::PathBuf::from(path);
                    <$detector as $crate::Detector>::check_dir(detector, &path, &mut host)
                        .map(|claim| claim.map(self::__backupcrawl_plugin_sdk_glue::claim_to_wit))
                        .map_err(|error| error.to_string())
                })
            }

            fn check_file(
                path: ::std::string::String,
            ) -> ::core::result::Result<
                ::core::option::Option<self::backupcrawl::detector::types::Claim>,
                ::std::string::String,
            > {
                __backupcrawl_with_detector(|detector| {
                    let mut host = self::__backupcrawl_plugin_sdk_glue::WasmHost;
                    let path = ::std::path::PathBuf::from(path);
                    <$detector as $crate::Detector>::check_file(detector, &path, &mut host)
                        .map(|claim| claim.map(self::__backupcrawl_plugin_sdk_glue::claim_to_wit))
                        .map_err(|error| error.to_string())
                })
            }

            fn check_symlink(
                link: self::backupcrawl::detector::types::LinkInfo,
            ) -> ::core::result::Result<
                ::core::option::Option<self::backupcrawl::detector::types::Claim>,
                ::std::string::String,
            > {
                __backupcrawl_with_detector(|detector| {
                    let mut host = self::__backupcrawl_plugin_sdk_glue::WasmHost;
                    let link = self::__backupcrawl_plugin_sdk_glue::link_info_from_wit(link);
                    <$detector as $crate::Detector>::check_symlink(detector, &link, &mut host)
                        .map(|claim| claim.map(self::__backupcrawl_plugin_sdk_glue::claim_to_wit))
                        .map_err(|error| error.to_string())
                })
            }

            /// Default bulk implementation: exactly N singular `check_file`
            /// calls (frozen decision, section 5.1 of the rewrite plan), via
            /// the `Detector::check_files` provided method.
            fn check_files(
                paths: ::std::vec::Vec<::std::string::String>,
            ) -> ::core::result::Result<
                ::std::vec::Vec<
                    ::core::option::Option<self::backupcrawl::detector::types::Claim>,
                >,
                ::std::string::String,
            > {
                __backupcrawl_with_detector(|detector| {
                    let mut host = self::__backupcrawl_plugin_sdk_glue::WasmHost;
                    let paths: ::std::vec::Vec<::std::path::PathBuf> =
                        paths.into_iter().map(::std::path::PathBuf::from).collect();
                    <$detector as $crate::Detector>::check_files(detector, &paths, &mut host)
                        .map(|claims| {
                            claims
                                .into_iter()
                                .map(|claim| {
                                    claim.map(self::__backupcrawl_plugin_sdk_glue::claim_to_wit)
                                })
                                .collect()
                        })
                        .map_err(|error| error.to_string())
                })
            }

            fn finish() -> ::core::result::Result<(), ::std::string::String> {
                __backupcrawl_with_detector(|detector| {
                    let mut host = self::__backupcrawl_plugin_sdk_glue::WasmHost;
                    <$detector as $crate::Detector>::finish(detector, &mut host)
                        .map_err(|error| error.to_string())
                })
            }
        }

        self::export!($detector);
    };
}
