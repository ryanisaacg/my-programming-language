use std::collections::HashMap;

use crate::{
    typecheck::{ExpressionType, TypeDeclaration},
    DeclarationContext,
};

use super::{HirModule, HirNode, HirNodeValue};

pub fn rewrite(module: &mut HirModule, declarations: &DeclarationContext) {
    module.par_visit_mut(|node| {
        node.walk_expected_types_for_children_mut(declarations, |expected_ty, child| {
            let ExpressionType::InstanceOf(expected_ty_id) = expected_ty else {
                return;
            };
            let Some(TypeDeclaration::Interface(expected_ty)) =
                declarations.id_to_decl.get(expected_ty_id)
            else {
                return;
            };

            let ExpressionType::InstanceOf(child_ty_id) = &child.ty else {
                return;
            };
            let Some(TypeDeclaration::Struct(child_ty)) = declarations.id_to_decl.get(child_ty_id)
            else {
                return;
            };

            let mut vtable = HashMap::new();
            for (name, func) in expected_ty.associated_functions.iter() {
                vtable.insert(*func, child_ty.associated_functions[name]);
            }
            let ty = ExpressionType::InstanceOf(expected_ty.id);

            let mut temp = HirNode::dummy();
            std::mem::swap(&mut temp, child);

            *child = HirNode::autogenerated(
                HirNodeValue::StructToInterface {
                    value: Box::new(temp),
                    vtable,
                },
                ty,
            );
        });
    });
}
