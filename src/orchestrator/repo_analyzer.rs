use std::collections::HashMap;
use std::path::Path;

use crate::router::ModelRouter;
use crate::types::{DetectedCommands, RepoAnalysis, RepoAnalysisConfig, TaskProfile, TechStack};

pub struct RepoAnalyzer {
    config: RepoAnalysisConfig,
}

impl RepoAnalyzer {
    pub fn new(config: RepoAnalysisConfig) -> Self {
        Self { config }
    }

    /// Full analysis pipeline: discover files → detect stack → detect commands → generate map.
    pub async fn analyze(
        &self,
        repo_path: &Path,
        router: &ModelRouter,
    ) -> anyhow::Result<RepoAnalysis> {
        let file_tree = self.discover_files(repo_path).await?;
        let key_files = self.find_key_files(&file_tree);
        let tech_stack = self
            .detect_tech_stack(repo_path, &file_tree, &key_files)
            .await?;
        let detected_commands = self
            .detect_build_commands(repo_path, &tech_stack, &key_files)
            .await?;
        let repo_map = self
            .generate_repo_map(repo_path, &file_tree, &tech_stack, &key_files, router)
            .await
            .unwrap_or_else(|_| self.fallback_repo_map(&file_tree, &key_files));

        Ok(RepoAnalysis {
            repo_path: repo_path.to_string_lossy().to_string(),
            tech_stack,
            detected_commands,
            repo_map,
            file_count: file_tree.len(),
            key_files,
        })
    }

    async fn discover_files(&self, repo_path: &Path) -> anyhow::Result<Vec<String>> {
        let output = tokio::process::Command::new("find")
            .args([
                repo_path.to_str().unwrap_or("."),
                "-type",
                "f",
                "-not",
                "-path",
                "*/.git/*",
                "-not",
                "-path",
                "*/node_modules/*",
                "-not",
                "-path",
                "*/target/*",
                "-not",
                "-path",
                "*/__pycache__/*",
                "-not",
                "-path",
                "*/.venv/*",
                "-not",
                "-path",
                "*/vendor/*",
                "-not",
                "-path",
                "*/.next/*",
                "-not",
                "-path",
                "*/dist/*",
                "-not",
                "-path",
                "*/build/*",
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .await?;

        let prefix = repo_path.to_str().unwrap_or("");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let files: Vec<String> = stdout
            .lines()
            .take(self.config.max_files_to_scan)
            .map(|l| {
                l.strip_prefix(prefix)
                    .unwrap_or(l)
                    .trim_start_matches('/')
                    .to_string()
            })
            .filter(|f| !f.is_empty())
            .collect();

        Ok(files)
    }

    fn find_key_files(&self, file_tree: &[String]) -> Vec<String> {
        let key_names = [
            "Cargo.toml",
            "package.json",
            "pyproject.toml",
            "setup.py",
            "setup.cfg",
            "go.mod",
            "Makefile",
            "CMakeLists.txt",
            "build.gradle",
            "pom.xml",
            "Gemfile",
            "requirements.txt",
            "Pipfile",
            "tsconfig.json",
            "README.md",
            "Dockerfile",
            "docker-compose.yml",
            "justfile",
            "Taskfile.yml",
        ];
        file_tree
            .iter()
            .filter(|f| key_names.iter().any(|k| f.ends_with(k)))
            .cloned()
            .collect()
    }

    async fn detect_tech_stack(
        &self,
        repo_path: &Path,
        file_tree: &[String],
        key_files: &[String],
    ) -> anyhow::Result<TechStack> {
        let mut ext_counts: HashMap<String, usize> = HashMap::new();
        for file in file_tree {
            if let Some(ext) = Path::new(file).extension().and_then(|e| e.to_str()) {
                *ext_counts.entry(ext.to_lowercase()).or_default() += 1;
            }
        }

        let total: usize = ext_counts.values().sum();
        let mut languages: Vec<(String, f32)> = ext_counts
            .iter()
            .map(|(ext, count)| {
                let lang = ext_to_language(ext);
                (lang, *count as f32 / total.max(1) as f32)
            })
            .filter(|(lang, _)| lang != "other")
            .collect();
        languages.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        languages.truncate(5);

        let primary_language = languages
            .first()
            .map(|(l, _)| l.clone())
            .unwrap_or_else(|| "unknown".to_string());

        let mut frameworks = Vec::new();
        for kf in key_files {
            let full_path = repo_path.join(kf);
            let content = match tokio::fs::read_to_string(&full_path).await {
                Ok(c) => c,
                Err(_) => continue,
            };

            if kf.ends_with("package.json") {
                if content.contains("\"react\"") {
                    frameworks.push("React".to_string());
                }
                if content.contains("\"next\"") {
                    frameworks.push("Next.js".to_string());
                }
                if content.contains("\"vue\"") {
                    frameworks.push("Vue".to_string());
                }
                if content.contains("\"express\"") {
                    frameworks.push("Express".to_string());
                }
                if content.contains("\"@nestjs") {
                    frameworks.push("NestJS".to_string());
                }
            }
            if kf.ends_with("Cargo.toml") {
                if content.contains("axum") {
                    frameworks.push("Axum".to_string());
                }
                if content.contains("actix") {
                    frameworks.push("Actix".to_string());
                }
                if content.contains("tokio") {
                    frameworks.push("Tokio".to_string());
                }
            }
            if kf.ends_with("pyproject.toml") || kf.ends_with("requirements.txt") {
                if content.contains("django") {
                    frameworks.push("Django".to_string());
                }
                if content.contains("fastapi") {
                    frameworks.push("FastAPI".to_string());
                }
                if content.contains("flask") {
                    frameworks.push("Flask".to_string());
                }
            }
        }

        let package_manager = if key_files.iter().any(|f| f.ends_with("Cargo.toml")) {
            Some("cargo".to_string())
        } else if key_files.iter().any(|f| f.ends_with("package-lock.json")) {
            Some("npm".to_string())
        } else if key_files.iter().any(|f| f.ends_with("yarn.lock")) {
            Some("yarn".to_string())
        } else if key_files.iter().any(|f| f.ends_with("pnpm-lock.yaml")) {
            Some("pnpm".to_string())
        } else if key_files.iter().any(|f| f.ends_with("package.json")) {
            Some("npm".to_string())
        } else if key_files.iter().any(|f| f.ends_with("go.mod")) {
            Some("go".to_string())
        } else if key_files.iter().any(|f| f.ends_with("Pipfile")) {
            Some("pipenv".to_string())
        } else if key_files.iter().any(|f| f.ends_with("pyproject.toml")) {
            Some("pip".to_string())
        } else {
            None
        };

        Ok(TechStack {
            primary_language,
            languages,
            frameworks,
            package_manager,
        })
    }

    async fn detect_build_commands(
        &self,
        repo_path: &Path,
        _tech_stack: &TechStack,
        key_files: &[String],
    ) -> anyhow::Result<DetectedCommands> {
        let mut lint = Vec::new();
        let mut build = Vec::new();
        let mut test = Vec::new();

        // Rust / Cargo
        if key_files.iter().any(|f| f == "Cargo.toml") {
            build.push("cargo build".to_string());
            test.push("cargo test".to_string());
            lint.push("cargo clippy -- -D warnings".to_string());
        }

        // Node / package.json
        if key_files.iter().any(|f| f == "package.json") {
            let pkg_path = repo_path.join("package.json");
            if let Ok(content) = tokio::fs::read_to_string(&pkg_path).await {
                if let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&content) {
                    if let Some(scripts) = pkg.get("scripts").and_then(|s| s.as_object()) {
                        if scripts.contains_key("lint") {
                            lint.push("npm run lint".to_string());
                        }
                        if scripts.contains_key("build") {
                            build.push("npm run build".to_string());
                        }
                        if scripts.contains_key("test") {
                            test.push("npm test".to_string());
                        }
                    }
                }
            }
        }

        // Python
        if key_files
            .iter()
            .any(|f| f.ends_with("pyproject.toml") || f.ends_with("setup.py"))
        {
            test.push("python -m pytest".to_string());
            if repo_path.join("ruff.toml").exists() || repo_path.join("pyproject.toml").exists() {
                lint.push("ruff check .".to_string());
            }
        }

        // Go
        if key_files.iter().any(|f| f == "go.mod") {
            build.push("go build ./...".to_string());
            test.push("go test ./...".to_string());
            lint.push("go vet ./...".to_string());
        }

        // Makefile targets
        if key_files.iter().any(|f| f == "Makefile") {
            let makefile_path = repo_path.join("Makefile");
            if let Ok(content) = tokio::fs::read_to_string(&makefile_path).await {
                for line in content.lines() {
                    if line.starts_with("lint:") || line.starts_with("check:") {
                        let target = line.split(':').next().unwrap_or("lint");
                        lint.push(format!("make {}", target));
                    }
                    if line.starts_with("test:") && test.is_empty() {
                        test.push("make test".to_string());
                    }
                    if line.starts_with("build:") && build.is_empty() {
                        build.push("make build".to_string());
                    }
                }
            }
        }

        Ok(DetectedCommands {
            lint_commands: lint,
            build_commands: build,
            test_commands: test,
        })
    }

    async fn generate_repo_map(
        &self,
        repo_path: &Path,
        file_tree: &[String],
        tech_stack: &TechStack,
        key_files: &[String],
        router: &ModelRouter,
    ) -> anyhow::Result<String> {
        let condensed_tree = self.condense_file_tree(file_tree);

        let mut key_contents = Vec::new();
        for kf in key_files.iter().take(5) {
            let full_path = repo_path.join(kf);
            if let Ok(content) = tokio::fs::read_to_string(&full_path).await {
                let truncated: String = content.chars().take(2000).collect();
                key_contents.push(format!("=== {} ===\n{}", kf, truncated));
            }
        }

        let prompt = format!(
            "Generate a concise repository map for an LLM agent that will modify this codebase.\n\n\
             TECH STACK: {} ({})\n\
             Frameworks: {}\n\n\
             FILE TREE (condensed):\n{}\n\n\
             KEY FILES:\n{}\n\n\
             Create a map that includes:\n\
             1. Project structure overview (1-2 sentences)\n\
             2. Key entry points and their purposes\n\
             3. Important directories and their roles\n\
             4. Configuration and build system details\n\
             5. Testing setup\n\n\
             Keep it under {} tokens. Be specific and actionable.",
            tech_stack.primary_language,
            tech_stack
                .languages
                .iter()
                .map(|(l, p)| format!("{}: {:.0}%", l, p * 100.0))
                .collect::<Vec<_>>()
                .join(", "),
            tech_stack.frameworks.join(", "),
            condensed_tree,
            key_contents.join("\n\n"),
            self.config.repo_map_max_tokens,
        );

        let constraints = crate::router::RoutingConstraints::for_profile(TaskProfile::Extraction);
        let (_decision, inference) = router
            .infer(TaskProfile::Extraction, &prompt, &constraints)
            .await?;

        Ok(inference.output)
    }

    fn condense_file_tree(&self, files: &[String]) -> String {
        let mut dir_files: HashMap<String, Vec<String>> = HashMap::new();
        for file in files {
            let dir = Path::new(file)
                .parent()
                .and_then(|p| p.to_str())
                .unwrap_or(".")
                .to_string();
            dir_files.entry(dir).or_default().push(
                Path::new(file)
                    .file_name()
                    .and_then(|f| f.to_str())
                    .unwrap_or(file)
                    .to_string(),
            );
        }

        let mut dirs: Vec<_> = dir_files.into_iter().collect();
        dirs.sort_by(|a, b| b.1.len().cmp(&a.1.len()));

        let mut output = String::new();
        for (dir, files) in dirs.iter().take(30) {
            let file_list = if files.len() > 10 {
                let shown: Vec<_> = files.iter().take(8).map(|f| f.as_str()).collect();
                format!("{} (+{} more)", shown.join(", "), files.len() - 8)
            } else {
                files.join(", ")
            };
            output.push_str(&format!("{}/: {}\n", dir, file_list));
        }
        if dirs.len() > 30 {
            output.push_str(&format!("... and {} more directories\n", dirs.len() - 30));
        }
        output
    }

    fn fallback_repo_map(&self, file_tree: &[String], key_files: &[String]) -> String {
        format!(
            "Repository contains {} files.\nKey files: {}\n\nFile tree:\n{}",
            file_tree.len(),
            key_files.join(", "),
            self.condense_file_tree(file_tree)
        )
    }
}

fn ext_to_language(ext: &str) -> String {
    match ext {
        "rs" => "Rust",
        "ts" | "tsx" => "TypeScript",
        "js" | "jsx" | "mjs" => "JavaScript",
        "py" => "Python",
        "go" => "Go",
        "java" => "Java",
        "rb" => "Ruby",
        "c" | "h" => "C",
        "cpp" | "cc" | "hpp" => "C++",
        "cs" => "C#",
        "swift" => "Swift",
        "kt" => "Kotlin",
        "html" | "htm" => "HTML",
        "css" | "scss" | "sass" => "CSS",
        "sql" => "SQL",
        "sh" | "bash" | "zsh" => "Shell",
        _ => "other",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ext_to_language_maps_common_extensions() {
        assert_eq!(ext_to_language("rs"), "Rust");
        assert_eq!(ext_to_language("ts"), "TypeScript");
        assert_eq!(ext_to_language("py"), "Python");
        assert_eq!(ext_to_language("xyz"), "other");
    }

    #[test]
    fn find_key_files_detects_manifests() {
        let files = vec![
            "src/main.rs".to_string(),
            "Cargo.toml".to_string(),
            "README.md".to_string(),
            "src/lib.rs".to_string(),
        ];
        let analyzer = RepoAnalyzer::new(RepoAnalysisConfig::default());
        let keys = analyzer.find_key_files(&files);
        assert!(keys.contains(&"Cargo.toml".to_string()));
        assert!(keys.contains(&"README.md".to_string()));
        assert!(!keys.contains(&"src/main.rs".to_string()));
    }

    #[test]
    fn condense_file_tree_groups_by_directory() {
        let files = vec![
            "src/main.rs".to_string(),
            "src/lib.rs".to_string(),
            "src/utils.rs".to_string(),
            "tests/test1.rs".to_string(),
        ];
        let analyzer = RepoAnalyzer::new(RepoAnalysisConfig::default());
        let condensed = analyzer.condense_file_tree(&files);
        assert!(condensed.contains("src/"));
        assert!(condensed.contains("tests/"));
    }
}
