//! Hoists lexically nested type declarations to qualified top-level
//! items, so every downstream pass sees the same flat shape the
//! qualified form (`struct Owner.Nested`) produces.

use koja_ast::ast::Item;

use crate::program::CheckedPackage;

pub(crate) fn desugar_packages(packages: &mut [CheckedPackage]) {
    for pkg in packages {
        for file in &mut pkg.files {
            let mut items = Vec::with_capacity(file.items.len());
            for item in file.items.drain(..) {
                hoist_item(item, &mut items);
            }
            file.items = items;
        }
    }
}

fn hoist_item(mut item: Item, out: &mut Vec<Item>) {
    let (owner_path, nested) = match &mut item {
        Item::Enum(decl) => (decl.path.clone(), std::mem::take(&mut decl.nested)),
        Item::Struct(decl) => (decl.path.clone(), std::mem::take(&mut decl.nested)),
        _ => {
            out.push(item);
            return;
        }
    };
    out.push(item);
    for mut nested_item in nested {
        match &mut nested_item {
            Item::Enum(decl) => prefix_path(&mut decl.path, &owner_path),
            Item::Struct(decl) => prefix_path(&mut decl.path, &owner_path),
            _ => {}
        }
        hoist_item(nested_item, out);
    }
}

fn prefix_path(path: &mut Vec<String>, owner: &[String]) {
    path.splice(0..0, owner.iter().cloned());
}
