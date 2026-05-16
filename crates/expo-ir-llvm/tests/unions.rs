//! IR-text snapshot tests for the union slice in
//! [`expo_ir_llvm::emit_script_llvm_ir`].
//!
//! Three contracts are pinned:
//!
//! - **Pre-emission types**: every union surfaces as a single named
//!   outer struct of shape `{ i8, [N x i8] }`, where `N` is the
//!   `IRUnionDecl.max_payload_size` computed by the IR layout pass.
//!   The mangle (`Union_<member-a>_or_<member-b>...`) is shared by
//!   every distinct surface spelling that canonicalizes to the
//!   same member set.
//! - **`UnionWrap` lowering**: a member-typed source flowing into a
//!   union slot lowers to alloca-the-outer + store the `i8` tag at
//!   field 0 + store the typed payload through field 1, then load
//!   the populated outer back as the SSA result.
//! - **Typed-binding lowering**: a `p: Member -> body` arm in a
//!   `match` over a union subject lowers to a tag-load + i8 `==`
//!   compare + conditional branch into a body block whose head
//!   reads the payload back through field 1, typed as the member.
//!
//! All assertions are substring-only because LLVM may shuffle
//! attribute ordering between patch versions.

use expo_ast::util::dedent;
use expo_ir_llvm::emit_script_llvm_ir;

mod common;

use common::{APP_NAME, assert_contains, assert_main_shape, lower_script_source as lower_script};

#[test]
fn union_outer_struct_emits_with_payload_buffer() {
    // The union outer is a named struct over `{ i8, [N x i8] }`
    // where `N` is the byte size of the largest member. With two
    // single-pointer-shaped members (`{ ptr }` for `Post` /
    // `Comment`), `N` is 8 on 64-bit hosts.
    let source = "
        struct Post
          title: String
        end

        struct Comment
          body: String
        end

        fn take(item: Post | Comment) -> Int
          match item
            _ -> 0
          end
        end

        take(Post{title: \"hi\"})
        ";
    let script = lower_script(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");
    assert_main_shape(&ir_text);
    // Mangle is order-canonical (alphabetical): Comment first.
    assert_contains(
        &ir_text,
        "%Union_TestApp.Comment_or_TestApp.Post = type { i8,",
    );
    // 8-byte payload buffer — pinned only on 64-bit hosts where
    // single-ptr fields measure 8 bytes; the IR layout pass uses
    // host pointer width.
    #[cfg(target_pointer_width = "64")]
    assert_contains(
        &ir_text,
        "%Union_TestApp.Comment_or_TestApp.Post = type { i8, [8 x i8] }",
    );
}

#[test]
fn union_wrap_emits_tag_store_then_payload_store() {
    // Member-typed arg flowing into a union slot stamps a
    // `Coercion::UnionWiden` at typecheck and lowers to a
    // matching `UnionWrap` at IR. The LLVM emit shape is
    // alloca-the-outer → GEP field 0 → store i8 tag → GEP field 1
    // → store the typed payload → load the outer back as the SSA
    // result. Substring-pinned on the tag store (`store i8`) and
    // the payload-pointer GEP into field 1.
    let source = "
        struct A
          x: Int
        end

        struct B
          y: Int
        end

        fn take(item: A | B) -> Int
          match item
            _ -> 0
          end
        end

        take(B{y: 5})
        ";
    let script = lower_script(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");
    // `B` is at canonical index 1 under alphabetical ordering, so
    // the tag store materializes `i8 1`. `A` would store `i8 0`.
    assert_contains(
        &ir_text,
        "store i8 1, ptr %Union_TestApp.A_or_TestApp.B_tag_ptr",
    );
    // GEP through the outer to field 1 (the payload buffer).
    assert_contains(
        &ir_text,
        "%Union_TestApp.A_or_TestApp.B_payload_ptr = getelementptr inbounds %Union_TestApp.A_or_TestApp.B, ptr %Union_TestApp.A_or_TestApp.B_tmp, i32 0, i32 1",
    );
}

#[test]
fn typed_binding_arm_emits_tag_load_compare_branch_and_payload_load() {
    // The `match` over a union subject with typed-binding arms
    // expands to per-arm tag tests followed by payload extracts in
    // the matched body block.
    let source = "
        struct A
          x: Int
        end

        struct B
          y: Int
        end

        fn label(item: A | B) -> Int
          match item
            a: A -> a.x
            b: B -> b.y
          end
        end

        label(A{x: 7})
        ";
    let script = lower_script(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");
    // Tag load is an `i8` read; the per-arm compare follows.
    assert_contains(
        &ir_text,
        "load i8, ptr %Union_TestApp.A_or_TestApp.B_tag_ptr",
    );
    // Tag-equality branches through `icmp eq i8` against the
    // canonical-order index (0 for `A`, 1 for `B`).
    assert_contains(&ir_text, "icmp eq i8");
    // Payload extraction reads through field 1 of the outer.
    assert_contains(
        &ir_text,
        "%Union_TestApp.A_or_TestApp.B_payload_ptr = getelementptr inbounds \
         %Union_TestApp.A_or_TestApp.B, ptr %Union_TestApp.A_or_TestApp.B_payload_src, \
         i32 0, i32 1",
    );
}

#[test]
fn struct_field_union_typechecks_and_emits() {
    // A struct field whose type is a union has to round-trip
    // through field-read → typed-binding match. Pre-emit ordering
    // declares the union outer before the struct body so the
    // struct's `{ ptr }` field can reference the union mangle.
    let source = "
        struct Cat
          name: String
        end

        struct Dog
          name: String
        end

        struct Holder
          pet: Cat | Dog
        end

        fn name_of(h: Holder) -> Int
          match h.pet
            c: Cat -> 1
            d: Dog -> 2
          end
        end

        name_of(Holder{pet: Cat{name: \"whiskers\"}})
        ";
    let script = lower_script(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");
    // The union outer is referenced as the type of the `Holder.pet` field.
    assert_contains(&ir_text, "%Union_TestApp.Cat_or_TestApp.Dog = type { i8,");
    // The struct body for `Holder` references the union outer for
    // the `pet` field — the type system requires the union to be
    // declared (and its layout registered) before this struct
    // body is emitted.
    assert_contains(
        &ir_text,
        "%TestApp.Holder = type { %Union_TestApp.Cat_or_TestApp.Dog }",
    );
}
