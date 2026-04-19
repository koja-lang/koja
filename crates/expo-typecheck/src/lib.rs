mod aliases;
mod check;
mod collect;
pub mod context;
mod cycle;
mod env;
mod expr;
mod pattern;
pub mod resolve;
mod stmt;
pub mod types;
mod validate;

use std::collections::BTreeSet;

use context::TypeContext;
use expo_ast::ast::Module;

pub use aliases::resolve_module_aliases;
pub use collect::{GlobalNames, collect_all_names};
pub use types::{Package, fqn_to_package, package_for_path, package_from_str};

/// Runs collection and type-checking in one step, returning a populated context.
/// Uses module-local names only (for single-file / test usage). The module's
/// file stem is used as the synthetic package name so bare-name lookups have
/// a deterministic scope.
pub fn check(module: &mut Module) -> TypeContext {
    let package = types::package_for_path(module.path.as_deref(), "__test__");
    let packages = BTreeSet::from([Package::Std, package_from_str(&package)]);
    let global = collect_all_names(&[module], packages);
    let mut ctx = collect::collect(module, &global, &package);
    resolve::resolve_packages(&mut ctx);
    check::check_module(module, &mut ctx, &package);
    validate::validate_resolved_types(module, &mut ctx);
    ctx
}

/// Validates all function bodies, expressions, and patterns against the context.
///
/// `package` is the module's package identifier (e.g. `"std"`, `"alpha"`, or
/// a synthetic file-stem-based name). Bare-name type references inside the
/// module are resolved within this package first before falling back to
/// globals.
pub fn check_module(module: &mut Module, ctx: &mut TypeContext, package: &str) {
    check::check_module(module, ctx, package);
}

/// Walks the AST to collect type signatures for functions, structs, and enums.
/// Requires [`GlobalNames`] from [`collect_all_names`] so that cross-module
/// type references resolve correctly on the first pass.
/// The `package` identifies which package the module belongs to (e.g. `"std"`,
/// `"json"`, or the project name from `expo.toml`).
pub fn collect_module(module: &Module, global_names: &GlobalNames, package: &str) -> TypeContext {
    collect::collect(module, global_names, package)
}

/// Synthesizes default protocol methods for impls whose protocols were unknown
/// during initial collection (e.g. after merging stdlib context).
pub fn synthesize_protocol_defaults(module: &Module, ctx: &mut TypeContext, package: &str) {
    collect::synthesize_protocol_defaults(module, ctx, package);
}

/// Detects recursive struct/enum fields and wraps them in [`types::Type::Indirect`]
/// for heap-allocated indirection.
pub fn mark_recursive_fields(ctx: &mut TypeContext) {
    cycle::mark_recursive_fields(ctx);
}

/// Auto-derives `Debug` protocol methods (`format`, `inspect`) on all struct
/// and enum types that don't already have them. Call after merging stdlib.
pub fn auto_derive_debug(ctx: &mut TypeContext) {
    collect::auto_derive_debug(ctx);
}

/// Resolves all `Package::Unresolved` identifiers in a [`TypeContext`] by
/// matching bare names against the type registry's map keys (which carry real
/// packages from collection). Must be called after merging and before checking.
pub fn resolve_packages(ctx: &mut TypeContext) {
    resolve::resolve_packages(ctx);
}

/// Walks every expression in the module and emits an error for any
/// `expr.resolved_type` that still carries `Package::Unresolved`. Call after
/// [`check_module`] to enforce the invariant that all types reaching codegen
/// are fully package-resolved.
pub fn validate_resolved_types(module: &Module, ctx: &mut TypeContext) {
    validate::validate_resolved_types(module, ctx);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Strips the leading newline and common indentation from a test string,
    /// so Expo source can be written as naturally indented blocks:
    ///
    /// ```ignore
    /// let ctx = check(&dedent("
    ///     fn main
    ///       x = 42
    ///     end
    /// "));
    /// ```
    fn dedent(s: &str) -> String {
        let s = s.strip_prefix('\n').unwrap_or(s);
        let min_indent = s
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.len() - l.trim_start().len())
            .min()
            .unwrap_or(0);
        s.lines()
            .map(|l| {
                if l.len() >= min_indent {
                    &l[min_indent..]
                } else {
                    l.trim()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn check_source(src: &str) -> TypeContext {
        let mut parse_result = expo_parser::parse(src);
        check(&mut parse_result.module)
    }

    fn errors(ctx: &TypeContext) -> Vec<&str> {
        ctx.diagnostics.iter().map(|d| d.message.as_str()).collect()
    }

    #[test]
    fn binary_literal_infers_binary_type() {
        let ctx = check_source(&dedent(
            "
            fn main
              x = <<0xFF, 0x00>>
            end
        ",
        ));
        assert!(errors(&ctx).is_empty(), "errors: {:?}", errors(&ctx));
    }

    #[test]
    fn binary_literal_non_byte_aligned_infers_bits() {
        let ctx = check_source(&dedent(
            "
            fn main
              x = <<1::3, 0::5>>
            end
        ",
        ));
        assert!(errors(&ctx).is_empty(), "errors: {:?}", errors(&ctx));
    }

    #[test]
    fn binary_literal_empty_infers_binary() {
        let ctx = check_source(&dedent(
            "
            fn main
              x = <<>>
            end
        ",
        ));
        assert!(errors(&ctx).is_empty(), "errors: {:?}", errors(&ctx));
    }

    #[test]
    fn binary_literal_overflow_detected() {
        let ctx = check_source(&dedent(
            "
            fn main
              x = <<256>>
            end
        ",
        ));
        assert!(
            errors(&ctx).iter().any(|e| e.contains("does not fit")),
            "expected overflow error, got: {:?}",
            errors(&ctx)
        );
    }

    #[test]
    fn binary_pattern_binds_int_for_sized_segment() {
        let ctx = check_source(&dedent(
            "
            fn main
              data: Binary = <<>>
              match data
                <<tag::8, _rest: Binary>> -> tag
                _ -> 0
              end
            end
        ",
        ));
        assert!(errors(&ctx).is_empty(), "errors: {:?}", errors(&ctx));
    }

    #[test]
    fn binary_pattern_requires_catch_all() {
        let ctx = check_source(&dedent(
            "
            fn main
              data: Binary = <<>>
              match data
                <<tag::8>> -> tag
              end
            end
        ",
        ));
        assert!(
            errors(&ctx).iter().any(|e| e.contains("catch-all")),
            "expected catch-all error, got: {:?}",
            errors(&ctx)
        );
    }

    #[test]
    fn binary_pattern_rejects_non_binary_subject() {
        let ctx = check_source(&dedent(
            "
            fn main
              x = 42
              match x
                <<tag::8>> -> tag
                _ -> 0
              end
            end
        ",
        ));
        assert!(
            errors(&ctx)
                .iter()
                .any(|e| e.contains("Binary") || e.contains("Bits")),
            "expected binary subject error, got: {:?}",
            errors(&ctx)
        );
    }

    #[test]
    fn binary_pattern_greedy_rest_must_be_last() {
        let ctx = check_source(&dedent(
            "
            fn main
              data: Binary = <<>>
              match data
                <<rest: Binary, tag::8>> -> tag
                _ -> 0
              end
            end
        ",
        ));
        assert!(
            errors(&ctx).iter().any(|e| e.contains("last segment")),
            "expected greedy-rest error, got: {:?}",
            errors(&ctx)
        );
    }

    // ---- Basic inference ----

    #[test]
    fn basic_int_assignment_no_errors() {
        let ctx = check_source(&dedent(
            "
            fn main
              x = 42
            end
        ",
        ));
        assert!(errors(&ctx).is_empty(), "errors: {:?}", errors(&ctx));
    }

    #[test]
    fn basic_string_assignment_no_errors() {
        let ctx = check_source(&dedent(
            r#"
            fn main
              x = "hello"
            end
        "#,
        ));
        assert!(errors(&ctx).is_empty(), "errors: {:?}", errors(&ctx));
    }

    #[test]
    fn basic_bool_assignment_no_errors() {
        let ctx = check_source(&dedent(
            "
            fn main
              x = true
              y = false
            end
        ",
        ));
        assert!(errors(&ctx).is_empty(), "errors: {:?}", errors(&ctx));
    }

    // ---- Type mismatch on return ----

    #[test]
    fn return_type_mismatch() {
        let ctx = check_source(&dedent(
            r#"
            fn greet -> Int
              "hello"
            end
        "#,
        ));
        assert!(
            errors(&ctx)
                .iter()
                .any(|e| e.contains("return type mismatch")),
            "expected return type mismatch, got: {:?}",
            errors(&ctx)
        );
    }

    #[test]
    fn return_type_correct() {
        let ctx = check_source(&dedent(
            "
            fn add(a: Int, b: Int) -> Int
              a + b
            end
        ",
        ));
        assert!(errors(&ctx).is_empty(), "errors: {:?}", errors(&ctx));
    }

    // ---- Undefined variable ----

    #[test]
    fn undefined_variable() {
        let ctx = check_source(&dedent(
            "
            fn main
              x = y
            end
        ",
        ));
        assert!(
            errors(&ctx).iter().any(|e| e.contains("unknown variable")),
            "expected unknown variable error, got: {:?}",
            errors(&ctx)
        );
    }

    // ---- Type annotation mismatch ----

    #[test]
    fn annotation_mismatch() {
        let ctx = check_source(&dedent(
            r#"
            fn main
              x: Int = "hello"
            end
        "#,
        ));
        assert!(
            errors(&ctx).iter().any(|e| e.contains("type mismatch")),
            "expected type mismatch, got: {:?}",
            errors(&ctx)
        );
    }

    #[test]
    fn annotation_correct() {
        let ctx = check_source(&dedent(
            "
            fn main
              x: Int = 42
            end
        ",
        ));
        assert!(errors(&ctx).is_empty(), "errors: {:?}", errors(&ctx));
    }

    // ---- Enum match exhaustiveness ----

    #[test]
    fn enum_match_exhaustive() {
        let ctx = check_source(&dedent(
            "
            enum Color
              Red
              Green
              Blue
            end

            fn describe(c: Color) -> Int
              match c
                Color.Red -> 1
                Color.Green -> 2
                Color.Blue -> 3
              end
            end
        ",
        ));
        assert!(errors(&ctx).is_empty(), "errors: {:?}", errors(&ctx));
    }

    #[test]
    fn enum_match_missing_variant() {
        let ctx = check_source(&dedent(
            "
            enum Color
              Red
              Green
              Blue
            end

            fn describe(c: Color) -> Int
              match c
                Color.Red -> 1
                Color.Green -> 2
              end
            end
        ",
        ));
        assert!(
            errors(&ctx)
                .iter()
                .any(|e| e.contains("non-exhaustive") || e.contains("missing")),
            "expected exhaustiveness error, got: {:?}",
            errors(&ctx)
        );
    }

    #[test]
    fn enum_match_with_catch_all() {
        let ctx = check_source(&dedent(
            "
            enum Color
              Red
              Green
              Blue
            end

            fn describe(c: Color) -> Int
              match c
                Color.Red -> 1
                _ -> 0
              end
            end
        ",
        ));
        assert!(errors(&ctx).is_empty(), "errors: {:?}", errors(&ctx));
    }

    // ---- Struct field access ----

    #[test]
    fn struct_valid_field_access() {
        let ctx = check_source(&dedent(
            "
            struct Point
              x: Int
              y: Int
            end

            fn get_x(p: Point) -> Int
              p.x
            end
        ",
        ));
        assert!(errors(&ctx).is_empty(), "errors: {:?}", errors(&ctx));
    }

    #[test]
    fn struct_invalid_field_access() {
        let ctx = check_source(&dedent(
            "
            struct Point
              x: Int
              y: Int
            end

            fn get_z(p: Point) -> Int
              p.z
            end
        ",
        ));
        assert!(
            errors(&ctx).iter().any(|e| e.contains("z")),
            "expected unknown field error, got: {:?}",
            errors(&ctx)
        );
    }

    // ---- Function arity ----

    #[test]
    fn function_wrong_arity() {
        let ctx = check_source(&dedent(
            "
            fn add(a: Int, b: Int) -> Int
              a + b
            end

            fn main
              add(1, 2, 3)
            end
        ",
        ));
        assert!(
            errors(&ctx).iter().any(|e| e.contains("argument")),
            "expected arity error, got: {:?}",
            errors(&ctx)
        );
    }

    // ---- Use after move ----

    #[test]
    fn use_after_move_struct() {
        let ctx = check_source(&dedent(
            "
            struct Box
              value: Int
            end

            fn consume(move b: Box) -> Int
              b.value
            end

            fn main
              b = Box { value: 1 }
              consume(b)
              consume(b)
            end
        ",
        ));
        assert!(
            errors(&ctx)
                .iter()
                .any(|e| e.contains("moved") || e.contains("move")),
            "expected use-after-move error, got: {:?}",
            errors(&ctx)
        );
    }

    #[test]
    fn copy_type_no_move_error() {
        let ctx = check_source(&dedent(
            "
            fn double(x: Int) -> Int
              x * 2
            end

            fn main
              n = 42
              double(n)
              double(n)
            end
        ",
        ));
        assert!(errors(&ctx).is_empty(), "errors: {:?}", errors(&ctx));
    }

    // ---- Constant type annotation ----

    #[test]
    fn const_no_errors() {
        let ctx = check_source(&dedent(
            "
            const MAX: Int = 100

            fn main
              x = MAX
            end
        ",
        ));
        assert!(errors(&ctx).is_empty(), "errors: {:?}", errors(&ctx));
    }
}
