use std::collections::HashMap;

use crate::{
    id::ID,
    typecheck::{ExpressionType, InterfaceType, StaticDeclaration, StructType},
};

use super::{HirModule, HirNode, HirNodeValue};

pub fn rewrite(module: &mut HirModule, declarations: &HashMap<ID, &StaticDeclaration>) {
    // TODO: this doesn't account for the parent being a function call

    // Find all places where a parent expects a child to be a struct
    let mut struct_expectations = HashMap::new();
    module.visit(|_, child| match &child.value {
        HirNodeValue::Int(_)
        | HirNodeValue::Float(_)
        | HirNodeValue::Bool(_)
        | HirNodeValue::Null
        | HirNodeValue::Parameter(_, _)
        | HirNodeValue::VariableReference(_)
        | HirNodeValue::Access(_, _)
        | HirNodeValue::Index(_, _)
        | HirNodeValue::CharLiteral(_)
        | HirNodeValue::StringLiteral(_)
        | HirNodeValue::TakeUnique(_)
        | HirNodeValue::TakeShared(_)
        | HirNodeValue::Dereference(_)
        | HirNodeValue::BinOp(_, _, _)
        | HirNodeValue::Declaration(_) => {}
        HirNodeValue::VtableCall(_, fn_id, params) => {
            let Some(StaticDeclaration::Func(func)) = declarations.get(fn_id) else {
                unreachable!();
            };
            for (i, ty) in func.params.iter().enumerate() {
                add_required_conversion(declarations, &mut struct_expectations, ty, &params[i]);
            }
        }
        HirNodeValue::Call(lhs, params) => {
            let ExpressionType::DeclaredType(id) = &lhs.ty else {
                unreachable!()
            };
            let Some(StaticDeclaration::Func(func)) = declarations.get(id) else {
                unreachable!()
            };
            for (i, ty) in func.params.iter().enumerate() {
                add_required_conversion(declarations, &mut struct_expectations, ty, &params[i]);
            }
        }
        HirNodeValue::Assignment(lhs, rhs) => {
            add_required_conversion(declarations, &mut struct_expectations, &lhs.ty, rhs);
        }
        HirNodeValue::Return(_) => {
            // TODO: returning interfaces from functions
        }
        HirNodeValue::If(_, _, _) | HirNodeValue::While(_, _) | HirNodeValue::Sequence(_) => {
            // TODO: returning interfaces from sequences / blocks
        }
        HirNodeValue::StructLiteral(ty_id, fields) => {
            let Some(StaticDeclaration::Struct(ty)) = declarations.get(ty_id) else {
                unreachable!();
            };
            for (name, field) in fields.iter() {
                add_required_conversion(
                    declarations,
                    &mut struct_expectations,
                    ty.fields.get(name).expect("field present"),
                    field,
                );
            }
        }
        // TODO: can entire collections be converted implicitly?
        HirNodeValue::ArrayLiteral(_) | HirNodeValue::ArrayLiteralLength(_, _) => {}
        HirNodeValue::StructToInterface { .. } => {
            todo!("can this even exist at this stage")
        }
    });
    module.visit_mut(|node| {
        let Some((node_ty, expected_ty)) = struct_expectations.get(&node.id) else {
            return;
        };
        let mut vtable = HashMap::new();
        for (name, func) in expected_ty.associated_functions.iter() {
            vtable.insert(
                func.id(),
                node_ty
                    .associated_functions
                    .get(name)
                    .expect("associated function to exist")
                    .id(),
            );
        }
        let ty = ExpressionType::DeclaredType(expected_ty.id);
        // Avoid stack overflow
        struct_expectations.remove(&node.id);

        let mut temp = HirNode::dummy();
        std::mem::swap(&mut temp, node);

        *node = HirNode::autogenerated(
            HirNodeValue::StructToInterface {
                value: Box::new(temp),
                vtable,
            },
            ty,
        );
    });
}

fn add_required_conversion<'a>(
    declarations: &HashMap<ID, &'a StaticDeclaration>,
    struct_expectations: &mut HashMap<ID, (&'a StructType, &'a InterfaceType)>,
    parent: &ExpressionType,
    child: &HirNode,
) {
    let ExpressionType::DeclaredType(parent_ty_id) = parent else {
        return;
    };
    let ExpressionType::DeclaredType(child_ty_id) = &child.ty else {
        return;
    };
    let Some(StaticDeclaration::Interface(parent_ty)) = declarations.get(parent_ty_id) else {
        return;
    };
    let Some(StaticDeclaration::Struct(child_ty)) = declarations.get(child_ty_id) else {
        return;
    };
    struct_expectations.insert(child.id, (child_ty, parent_ty));
}
