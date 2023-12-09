use std::collections::HashMap;

use crate::{
    id::ID,
    typecheck::{ExpressionType, InterfaceType, StaticDeclaration, StructType},
};

use super::{IrModule, IrNode, IrNodeValue};

pub fn rewrite<'ast>(module: &mut IrModule, declarations: &HashMap<ID, &'ast StaticDeclaration>) {
    // TODO: this doesn't account for the parent being a function call

    // Find all places where a parent expects a child to be a struct
    let mut struct_expectations = HashMap::new();
    module.visit(|_, child| match &child.value {
        IrNodeValue::Int(_)
        | IrNodeValue::Float(_)
        | IrNodeValue::Bool(_)
        | IrNodeValue::Null
        | IrNodeValue::Parameter(_, _)
        | IrNodeValue::VariableReference(_)
        | IrNodeValue::Access(_, _)
        | IrNodeValue::Index(_, _)
        | IrNodeValue::CharLiteral(_)
        | IrNodeValue::StringLiteral(_)
        | IrNodeValue::TakeUnique(_)
        | IrNodeValue::TakeShared(_)
        | IrNodeValue::Dereference(_)
        | IrNodeValue::BinOp(_, _, _)
        | IrNodeValue::Declaration(_) => {}
        IrNodeValue::VtableCall(_, fn_id, params) => {
            let Some(StaticDeclaration::Func(func)) = declarations.get(fn_id) else {
                unreachable!();
            };
            for (i, ty) in func.params.iter().enumerate() {
                add_required_conversion(declarations, &mut struct_expectations, ty, &params[i]);
            }
        }
        IrNodeValue::Call(lhs, params) => {
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
        IrNodeValue::Assignment(lhs, rhs) => {
            add_required_conversion(declarations, &mut struct_expectations, &lhs.ty, &rhs);
        }
        IrNodeValue::Return(_) => {
            // TODO: returning interfaces from functions
        }
        IrNodeValue::If(_, _, _) | IrNodeValue::While(_, _) | IrNodeValue::Sequence(_) => {
            // TODO: returning interfaces from sequences / blocks
        }
        IrNodeValue::StructLiteral(ty_id, fields) => {
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
        IrNodeValue::ArrayLiteral(_) | IrNodeValue::ArrayLiteralLength(_, _) => todo!("arrays"),
        IrNodeValue::StructToInterface { .. } => {
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

        let mut temp = IrNode::dummy();
        std::mem::swap(&mut temp, node);

        *node = IrNode {
            id: ID::new(),
            value: IrNodeValue::StructToInterface {
                value: Box::new(temp),
                vtable,
            },
            ty,
        };
    });
}

fn add_required_conversion<'a>(
    declarations: &HashMap<ID, &'a StaticDeclaration>,
    struct_expectations: &mut HashMap<ID, (&'a StructType, &'a InterfaceType)>,
    parent: &ExpressionType,
    child: &IrNode,
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
