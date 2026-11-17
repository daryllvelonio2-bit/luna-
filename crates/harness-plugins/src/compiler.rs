//! In-process TypeScript -> JavaScript transpiler backed by `oxc_transformer`.
//!
//! Compiles DeepSeek-style `src/index.ts` plugins into ES2022 JavaScript (`dist/index.js`)
//! runnable directly inside the embedded QuickJS engine without Node.js or `tsc`.

use std::path::Path;

use oxc_allocator::Allocator;
use oxc_codegen::{Codegen, CodegenOptions, IndentChar};
use oxc_parser::Parser;
use oxc_semantic::SemanticBuilder;
use oxc_span::SourceType;
use oxc_transformer::{TransformOptions, Transformer, TypeScriptOptions};

#[derive(Debug, thiserror::Error)]
pub enum CompileError {
    #[error("TypeScript syntax error in {file}:\n{details}")]
    Parse { file: String, details: String },
    #[error("TypeScript transform error in {file}:\n{details}")]
    Transform { file: String, details: String },
}

/// Compile TypeScript source (`source_name`, e.g. `"src/index.ts"`) into ES module JavaScript.
pub fn compile_typescript(source_name: &str, ts_source: &str) -> Result<String, CompileError> {
    let allocator = Allocator::default();
    let path = Path::new(source_name);
    let source_type = SourceType::from_path(path).unwrap_or_else(|_| SourceType::ts());

    let parsed = Parser::new(&allocator, ts_source, source_type).parse();
    if !parsed.diagnostics.is_empty() {
        let details = parsed
            .diagnostics
            .iter()
            .map(|d| d.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        return Err(CompileError::Parse {
            file: source_name.to_string(),
            details,
        });
    }

    let mut program = parsed.program;
    let semantic_ret = SemanticBuilder::new()
        .with_excess_capacity(2.0)
        .with_enum_eval(true)
        .build(&program);

    let scoping = semantic_ret.semantic.into_scoping();
    let transform_options = TransformOptions {
        typescript: TypeScriptOptions::default(),
        ..TransformOptions::default()
    };

    let tr = Transformer::new(&allocator, path, &transform_options)
        .build_with_scoping(scoping, &mut program);
    if !tr.diagnostics.is_empty() {
        let details = tr
            .diagnostics
            .iter()
            .map(|d| d.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        return Err(CompileError::Transform {
            file: source_name.to_string(),
            details,
        });
    }

    let codegen_options = CodegenOptions {
        indent_char: IndentChar::Space,
        indent_width: 2,
        ..CodegenOptions::default()
    };
    let printed = Codegen::new()
        .with_options(codegen_options)
        .build(&program)
        .code;

    Ok(printed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transpiles_deepseek_style_typescript_plugin() {
        let ts = r#"
import type { Context } from '@luna/sdk';

export const name = 'demo-plugin';

interface DemoConfig {
    greeting: string;
}

export function apply(ctx: Context): void {
    const cfg: DemoConfig = { greeting: 'hello' };
    ctx.tools.register({
        name: 'say_hello',
        description: 'Say hello',
        parameters: { type: 'object', properties: {} }
    }, (_args: Record<string, unknown>) => {
        return cfg.greeting;
    });
}
"#;
        let js = compile_typescript("src/index.ts", ts).expect("compile ts");
        assert!(!js.contains("interface DemoConfig"));
        assert!(!js.contains("import type"));
        assert!(js.contains("export function apply(ctx)"));
    }

    #[test]
    fn rejects_invalid_typescript_syntax() {
        let bad = "export function apply(ctx: { broken";
        let err = compile_typescript("src/index.ts", bad).unwrap_err();
        assert!(err.to_string().contains("syntax error"));
    }
}
