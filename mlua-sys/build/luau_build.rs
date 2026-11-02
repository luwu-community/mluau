use std::env;
use std::fs;
use std::path::{Path, PathBuf};

// Base luau version targetted by this luwu submodule
pub const LUAU_VERSION: usize = 730;

// Max cstack slots
pub const LUAU_MAX_C_SLOTS: usize = 1000000;

/// Represents the configuration for building Luau artifacts.
pub struct Build {
    out_dir: Option<PathBuf>,
    target: Option<String>,
    host: Option<String>,
    // Enable code generator (jit)
    enable_codegen: bool,
    // Build the type checker and its C API (luaanalysis.h)
    enable_analysis: bool,
    // Vector size, must be 3 (default) or 4
    vector_size: usize,
}

/// Represents the artifacts produced by the build process.
pub struct Artifacts {
    lib_dir: PathBuf,
    libs: Vec<String>,
    cpp_stdlib: Option<String>,
}

impl Default for Build {
    fn default() -> Self {
        Build {
            out_dir: env::var_os("OUT_DIR").map(PathBuf::from),
            target: env::var("TARGET").ok(),
            host: env::var("HOST").ok(),
            enable_codegen: false,
            enable_analysis: false,
            vector_size: 3,
        }
    }
}

impl Build {
    /// Creates a new `Build` instance with default settings.
    pub fn new() -> Build {
        Build::default()
    }

    /// Sets whether to enable the code generator (JIT).
    ///
    /// Default is false.
    pub fn enable_codegen(&mut self, enable: bool) -> &mut Build {
        self.enable_codegen = enable;
        self
    }

    /// Sets whether to build the type checker and its C API.
    ///
    /// Default is false.
    pub fn enable_analysis(&mut self, enable: bool) -> &mut Build {
        self.enable_analysis = enable;
        self
    }

    /// Sets the vector size (3 or 4).
    ///
    /// Default is 3.
    pub fn set_vector_size(&mut self, size: usize) -> &mut Build {
        assert!(size == 3 || size == 4, "vector size must be 3 or 4");
        self.vector_size = size;
        self
    }

    /// Builds the Lua artifacts for the specified version.
    pub fn build(&mut self) -> Artifacts {
        let target = &self.target.as_ref().expect("TARGET is not set")[..];
        let host = &self.host.as_ref().expect("HOST is not set")[..];
        let out_dir = self.out_dir.as_ref().expect("OUT_DIR is not set");
        let build_dir = out_dir.join("luau-build");

        let source_base_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let luau_source_dir = source_base_dir.join("../luwu");
        let common_include_dir = luau_source_dir.join("Common").join("include");
        let vm_source_dir = luau_source_dir.join("VM").join("src");
        let vm_include_dir = luau_source_dir.join("VM").join("include");

        if !luau_source_dir.exists() {
            panic!("luau_source_dir does not exist")
        }

        // Cleanup
        if build_dir.exists() {
            fs::remove_dir_all(&build_dir).unwrap();
        }

        // Configure C++
        let mut config = cc::Build::new();
        config
            .warnings(false)
            .cargo_metadata(false)
            .std("c++17")
            .cpp(true);

        if target.ends_with("emscripten") {
            // cc-rs unconditionally adds `-fno-exceptions` for wasm32.
            // We need to re-enable exceptions and switch to the
            // wasm-native EH ABI that Rust's emscripten target uses.
            config.flag_if_supported("-fexceptions");
            config.flag_if_supported("-fwasm-exceptions");
        }

        // Common defines
        config.define("LUAI_MAXCSTACK", Some(LUAU_MAX_C_SLOTS.to_string().as_str()));
        config.define("LUA_VECTOR_SIZE", &*self.vector_size.to_string());
        config.define("LUA_API", "extern \"C\"");

        if cfg!(debug_assertions) {
            config.define("LUAU_ENABLE_ASSERT", None);
        } else {
            // this flag allows compiler to lower sqrt() into a single CPU instruction
            config.flag_if_supported("-fno-math-errno");
        }

        config.include(&common_include_dir);

        // Build `Ast` library
        let ast_lib_name = "luauast";
        let ast_source_dir = luau_source_dir.join("Ast").join("src");
        let ast_include_dir = luau_source_dir.join("Ast").join("include");
        assert!(ast_source_dir.exists());
        config
            .clone()
            .include(&ast_include_dir)
            .add_files_by_ext_sorted(&ast_source_dir, "cpp")
            .out_dir(&build_dir)
            .compile(ast_lib_name);

        // Build `CodeGen` library
        let codegen_lib_name = "luaucodegen";
        let codegen_source_dir = luau_source_dir.join("CodeGen").join("src");
        let codegen_include_dir = luau_source_dir.join("CodeGen").join("include");
        if self.enable_codegen {
            if target.ends_with("emscripten") {
                panic!("codegen (jit) is not supported on emscripten");
            }

            config
                .clone()
                .include(&codegen_include_dir)
                .include(&vm_include_dir)
                .include(&vm_source_dir)
                .define("LUACODEGEN_API", "extern \"C\"")
                .add_files_by_ext_sorted(&codegen_source_dir, "cpp")
                .out_dir(&build_dir)
                .compile(codegen_lib_name);
        }

        // Build `Common` library
        let common_lib_name = "luaucommon";
        let common_source_dir = luau_source_dir.join("Common").join("src");
        config
            .clone()
            .add_files_by_ext_sorted(&common_source_dir, "cpp")
            .out_dir(&build_dir)
            .compile(common_lib_name);

        // Build `Bytecode` library
        let bytecode_lib_name = "luaubytecode";
        let bytecode_source_dir = luau_source_dir.join("Bytecode").join("src");
        let bytecode_include_dir = luau_source_dir.join("Bytecode").join("include");
        config
            .clone()
            .include(&bytecode_include_dir)
            .add_files_by_ext_sorted(&bytecode_source_dir, "cpp")
            .out_dir(&build_dir)
            .compile(bytecode_lib_name);

        // Build `Compiler` library
        let compiler_lib_name = "luaucompiler";
        let compiler_source_dir = luau_source_dir.join("Compiler").join("src");
        let compiler_include_dir = luau_source_dir.join("Compiler").join("include");
        config
            .clone()
            .include(&compiler_include_dir)
            .include(&bytecode_include_dir)
            .include(&ast_include_dir)
            .define("LUACODE_API", "extern \"C\"")
            .add_files_by_ext_sorted(&compiler_source_dir, "cpp")
            .out_dir(&build_dir)
            .compile(compiler_lib_name);

        // Build `Config` library
        let config_lib_name = "luauconfig";
        let config_source_dir = luau_source_dir.join("Config").join("src");
        let config_include_dir = luau_source_dir.join("Config").join("include");
        config
            .clone()
            .include(&config_include_dir)
            .include(&ast_include_dir)
            .include(&compiler_include_dir)
            .include(&vm_include_dir)
            .add_files_by_ext_sorted(&config_source_dir, "cpp")
            .out_dir(&build_dir)
            .compile(config_lib_name);

        // Build `Require` library
        let require_lib_name = "luaurequire";
        let require_source_dir = luau_source_dir.join("Require").join("src");
        let require_include_dir = luau_source_dir.join("Require").join("include");
        config
            .clone()
            .include(&require_include_dir)
            .include(&ast_include_dir)
            .include(&config_include_dir)
            .include(&vm_include_dir)
            .add_files_by_ext_sorted(&require_source_dir, "cpp")
            .out_dir(&build_dir)
            .compile(require_lib_name);

        // Build VM
        let vm_lib_name = "luauvm";
        config
            .clone()
            .include(&vm_include_dir)
            .add_files_by_ext_sorted(&vm_source_dir, "cpp")
            .out_dir(&build_dir)
            .compile(vm_lib_name);

        let mut artifacts = Artifacts {
            lib_dir: build_dir,
            libs: vec![
                vm_lib_name.to_string(),
                bytecode_lib_name.to_string(),
                compiler_lib_name.to_string(),
                ast_lib_name.to_string(),
                common_lib_name.to_string(),
                config_lib_name.to_string(),
                require_lib_name.to_string(),
            ],
            cpp_stdlib: Self::get_cpp_link_stdlib(target, host),
        };

        if self.enable_codegen {
            artifacts.libs.push(codegen_lib_name.to_string());
        }

        if self.enable_analysis {
            let analysis_lib_name = "luauanalysis";
            let analysis_include_dir = luau_source_dir.join("Analysis").join("include");
            let analysis_api_include_dir = luau_source_dir.join("AnalysisApi").join("include");
            let cli_include_dir = luau_source_dir.join("CLI").join("include");
            let cli_source_dir = luau_source_dir.join("CLI").join("src");

            let mut analysis_config = config.clone();
            analysis_config
                .include(&analysis_include_dir)
                .include(&analysis_api_include_dir)
                .include(&cli_include_dir)
                .include(&ast_include_dir)
                .include(&config_include_dir)
                .include(&compiler_include_dir)
                .include(&bytecode_include_dir)
                .include(&require_include_dir)
                .include(&vm_include_dir)
                .define("LUAANALYSIS_API", "extern \"C\"")
                .add_files_by_ext_sorted(&luau_source_dir.join("Analysis").join("src"), "cpp")
                .add_files_by_ext_sorted(&luau_source_dir.join("AnalysisApi").join("src"), "cpp");

            // Only the CLI files the C API uses: luwu-analyze's file and config resolvers and what they need
            for file in ["AnalyzeRequirer.cpp", "AnalyzeResolvers.cpp", "FileUtils.cpp", "Flags.cpp", "VfsNavigator.cpp"] {
                analysis_config.file(cli_source_dir.join(file));
            }

            analysis_config.out_dir(&artifacts.lib_dir).compile(analysis_lib_name);

            // Before the libraries it uses: a static library only resolves symbols from libraries listed after it
            artifacts.libs.insert(0, analysis_lib_name.to_string());
        }

        artifacts
    }

    /// Returns the C++ standard library:
    /// 1) Uses `CXXSTDLIB` environment variable if set
    /// 2) The default `c++` for OS X and BSDs
    /// 3) `c++_shared` for Android
    /// 4) `None` for MSVC
    /// 5) `stdc++` for anything else.
    ///
    /// Inspired by the `cc` crate.
    fn get_cpp_link_stdlib(target: &str, host: &str) -> Option<String> {
        // Try to get value from the `CXXSTDLIB` env variable
        let kind = if host == target { "HOST" } else { "TARGET" };
        let res = env::var(format!("CXXSTDLIB_{target}"))
            .or_else(|_| env::var(format!("CXXSTDLIB_{}", target.replace('-', "_"))))
            .or_else(|_| env::var(format!("{kind}_CXXSTDLIB")))
            .or_else(|_| env::var("CXXSTDLIB"))
            .ok();
        if res.is_some() {
            return res;
        }

        if target.contains("msvc") {
            None
        } else if target.contains("apple") | target.contains("freebsd") | target.contains("openbsd")
        {
            Some("c++".to_string())
        } else if target.contains("android") {
            Some("c++_shared".to_string())
        } else {
            Some("stdc++".to_string())
        }
    }
}

impl Artifacts {
    pub fn print_cargo_metadata(&self) {
        println!("cargo:rustc-link-search=native={}", self.lib_dir.display());
        for lib in &self.libs {
            println!("cargo:rustc-link-lib=static={lib}");
        }
        if let Some(ref cpp_stdlib) = self.cpp_stdlib {
            println!("cargo:rustc-link-lib={cpp_stdlib}");
        }
        println!("cargo:rustc-env=LUAU_VERSION=0.{LUAU_VERSION}");
    }
}

trait AddFilesByExt {
    fn add_files_by_ext_sorted(&mut self, dir: &Path, ext: &str) -> &mut Self;
}

impl AddFilesByExt for cc::Build {
    // It's important to keep the order of the files to get consistent builds between machines
    // if the order is not always the same, the final binary produces a different SHA256 which
    // might cause issues if one needs to verify which binary is being executed
    fn add_files_by_ext_sorted(&mut self, dir: &Path, ext: &str) -> &mut Self {
        let mut sources: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension() == Some(ext.as_ref()))
            .map(|e| e.path())
            .collect();

        // Sort for determinism
        sources.sort();

        for source in sources {
            self.file(source);
        }

        self
    }
}
