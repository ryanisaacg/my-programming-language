use std::collections::HashMap;

use crate::{
    id::ID,
    typecheck::{ExpressionType, InterfaceType, StaticDeclaration, StructType},
};

use super::{IrNode, IrNodeValue};

pub fn rewrite(declarations: &HashMap<ID, &StaticDeclaration>, root: &mut IrNode) {
    // We only care about function calls that are an access on the left hand side
    let IrNodeValue::Call(call_lhs, args) = &mut root.value else {
        return;
    };
    let IrNodeValue::Access(lhs, func_name) = &mut call_lhs.value else {
        return;
    };
    let ExpressionType::DeclaredType(ty_id) = &lhs.ty else {
        panic!("lhs of access must be a user-declared type");
    };
    match declarations.get(ty_id) {
        Some(StaticDeclaration::Struct(StructType {
            associated_functions,
            ..
        })) => {
            if let Some(func) = associated_functions.get(func_name) {
                // Replace the existing left hand side with a reference to the called function
                let mut temporary = IrNode {
                    id: ID::new(),
                    value: IrNodeValue::VariableReference(func.id()),
                    ty: func.expr(),
                };
                std::mem::swap(&mut temporary, call_lhs);
                // Insert the struct as a parameter to the newly called function
                let call_lhs = temporary;
                let IrNodeValue::Access(lhs, _) = call_lhs.value else {
                    unreachable!();
                };
                args.insert(0, *lhs);
            }
        }
        Some(StaticDeclaration::Interface(InterfaceType {
            associated_functions,
            ..
        })) => {
            if let Some(func) = associated_functions.get(func_name) {
                // Insert the interface as a parameter to itself
                args.insert(0, IrNode::clone(lhs));

                let mut temp = IrNode::dummy();
                std::mem::swap(&mut temp, root);
                let IrNode {
                    id,
                    value: IrNodeValue::Call(lhs, args),
                    ty,
                } = temp
                else {
                    unreachable!()
                };
                let IrNodeValue::Access(lhs, _func_name) = lhs.value else {
                    unreachable!()
                };
                temp = IrNode {
                    id,
                    value: IrNodeValue::VtableCall(lhs, func.id(), args),
                    ty,
                };
                std::mem::swap(&mut temp, root);
            }
        }
        _ => {}
    }
}
