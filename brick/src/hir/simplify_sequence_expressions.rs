use crate::{
    id::{NodeID, VariableID},
    provenance::SourceRange,
    typecheck::ExpressionType,
    DeclarationContext,
};

use super::{HirModule, HirNode, HirNodeValue};

/**
 * Wherever a sequence (or an if statement) is assigned to a variable, replace it with a sequence
 * that ends in that variable's assignment (e.g. x = if a { 1 } else { 2 } should become if a { x =
 * 1 } else { x = 2 })
 */
pub fn simplify_sequence_assignments(module: &mut HirModule) {
    module.par_visit_mut(|node| {
        let HirNodeValue::Assignment(lhs, rhs) = &mut node.value else {
            return;
        };
        match &mut rhs.value {
            HirNodeValue::Sequence(values) => {
                let mut swapped_lhs = HirNode::dummy();
                std::mem::swap(&mut swapped_lhs, lhs);
                replace_last_with_assignment(node.provenance.clone(), values, swapped_lhs);
            }
            HirNodeValue::If(_, if_branch, Some(else_branch)) => {
                let mut swapped_lhs = HirNode::dummy();
                std::mem::swap(&mut swapped_lhs, lhs);
                let HirNodeValue::Sequence(if_branch) = &mut if_branch.value else {
                    unreachable!()
                };
                let HirNodeValue::Sequence(else_branch) = &mut else_branch.value else {
                    unreachable!()
                };
                replace_last_with_assignment(
                    node.provenance.clone(),
                    if_branch,
                    swapped_lhs.clone(),
                );
                replace_last_with_assignment(node.provenance.clone(), else_branch, swapped_lhs);
            }
            HirNodeValue::Switch { value: _, cases } => {
                let mut swapped_lhs = HirNode::dummy();
                std::mem::swap(&mut swapped_lhs, lhs);
                for case in cases.iter_mut() {
                    let HirNodeValue::Sequence(case) = &mut case.value else {
                        unreachable!()
                    };
                    replace_last_with_assignment(
                        node.provenance.clone(),
                        case,
                        swapped_lhs.clone(),
                    );
                }
            }
            _ => return,
        }
        rhs.ty = ExpressionType::Void;
        let mut temp = HirNode::dummy();
        std::mem::swap(&mut temp, rhs);
        *node = temp;
    });
}

fn replace_last_with_assignment(
    provenance: Option<SourceRange>,
    values: &mut [HirNode],
    lhs: HirNode,
) {
    let mut new_value = HirNode::dummy();
    std::mem::swap(values.last_mut().unwrap(), &mut new_value);
    std::mem::swap(
        values.last_mut().unwrap(),
        &mut HirNode {
            id: NodeID::new(),
            value: HirNodeValue::Assignment(Box::new(lhs), Box::new(new_value)),
            ty: ExpressionType::Void,
            provenance,
        },
    );
}

pub fn simplify_sequence_uses(module: &mut HirModule, declarations: &DeclarationContext) {
    module.par_visit_mut(|node| {
        let mut temporaries = Vec::new();
        node.walk_expected_types_for_children_mut(declarations, |ty, child| {
            if !matches!(
                &child.value,
                HirNodeValue::Sequence(_) | HirNodeValue::If(_, _, _) | HirNodeValue::Switch { .. },
            ) {
                return;
            }

            let temp_id = VariableID::new();
            let lhs =
                HirNode::autogenerated(HirNodeValue::VariableReference(temp_id.into()), ty.clone());

            match &mut child.value {
                HirNodeValue::Sequence(values) => {
                    replace_last_with_assignment(child.provenance.clone(), values, lhs.clone());
                }
                HirNodeValue::If(_, if_branch, Some(else_branch)) => {
                    let HirNodeValue::Sequence(if_branch) = &mut if_branch.value else {
                        unreachable!()
                    };
                    let HirNodeValue::Sequence(else_branch) = &mut else_branch.value else {
                        unreachable!()
                    };
                    replace_last_with_assignment(child.provenance.clone(), if_branch, lhs.clone());
                    replace_last_with_assignment(
                        child.provenance.clone(),
                        else_branch,
                        lhs.clone(),
                    );
                }
                HirNodeValue::Switch { value: _, cases } => {
                    for case in cases.iter_mut() {
                        let HirNodeValue::Sequence(case) = &mut case.value else {
                            unreachable!()
                        };
                        replace_last_with_assignment(child.provenance.clone(), case, lhs.clone());
                    }
                }
                _ => unreachable!(),
            }
            child.ty = ExpressionType::Void;

            let mut temp = lhs;
            std::mem::swap(child, &mut temp);

            temporaries.push(HirNode::autogenerated(
                HirNodeValue::Declaration(temp_id),
                ty.clone(),
            ));
            temporaries.push(temp);
        });

        if !temporaries.is_empty() {
            temporaries.push(HirNode {
                id: node.id,
                value: std::mem::take(&mut node.value),
                provenance: node.provenance.clone(),
                ty: node.ty.clone(),
            });
            node.value = HirNodeValue::Sequence(temporaries);
        }
    });
}

pub fn simplify_trailing_if(module: &mut HirModule) {
    module.par_visit_mut(|node| {
        let HirNodeValue::Sequence(children) = &mut node.value else {
            return;
        };
        if !matches!(
            children.last(),
            Some(HirNode {
                value: HirNodeValue::If(_, _, Some(_)) | HirNodeValue::Switch { .. },
                ..
            }),
        ) {
            return;
        }
        let trailing_statement = children.last_mut().unwrap();
        if trailing_statement.ty == ExpressionType::Void
            || trailing_statement.ty == ExpressionType::Unreachable
        {
            return;
        }
        let ty = trailing_statement.ty.clone();

        // Rewrite if statement use a temporary variable in both branches
        let temporary_var_id = VariableID::new();
        let lhs = HirNode::autogenerated(
            HirNodeValue::VariableReference(temporary_var_id.into()),
            ty.clone(),
        );

        match &mut trailing_statement.value {
            HirNodeValue::If(_, if_branch, Some(else_branch)) => {
                let HirNodeValue::Sequence(if_branch_children) = &mut if_branch.value else {
                    unreachable!()
                };
                let HirNodeValue::Sequence(else_branch_children) = &mut else_branch.value else {
                    unreachable!()
                };
                replace_last_with_assignment(
                    if_branch.provenance.clone(),
                    if_branch_children,
                    lhs.clone(),
                );
                replace_last_with_assignment(
                    else_branch.provenance.clone(),
                    else_branch_children,
                    lhs.clone(),
                );
            }
            HirNodeValue::Switch { value: _, cases } => {
                for case in cases.iter_mut() {
                    let HirNodeValue::Sequence(case_children) = &mut case.value else {
                        unreachable!()
                    };
                    replace_last_with_assignment(
                        case.provenance.clone(),
                        case_children,
                        lhs.clone(),
                    );
                }
            }
            _ => unreachable!(),
        }

        trailing_statement.ty = ExpressionType::Void;
        children.insert(
            children.len() - 1,
            HirNode::autogenerated(HirNodeValue::Declaration(temporary_var_id), ty.clone()),
        );
        children.push(HirNode::autogenerated(
            HirNodeValue::VariableReference(temporary_var_id.into()),
            ty,
        ));
    });
}
