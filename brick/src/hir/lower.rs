use super::{
    ArithmeticOp, BinaryLogicalOp, ComparisonOp, GeneratorProperties, HirFunction, HirModule,
    HirNode, HirNodeValue, UnaryLogicalOp,
};

use crate::{
    declaration_context::FileDeclarations,
    id::{NodeID, VariableID},
    parser::{AstArena, AstNode, AstNodeValue, BinOp, IfDeclaration, UnaryOp},
    typecheck::{
        fully_dereference, shallow_dereference, traverse_dots, CollectionType, ExpressionType,
        FuncType, PointerKind, PrimitiveType, TypeDeclaration, TypecheckedFile,
        TypecheckedFunction,
    },
    DeclarationContext,
};

pub fn lower_module(
    declarations: &DeclarationContext,
    ast: &AstArena,
    module: TypecheckedFile<'_, '_>,
) -> HirModule {
    let TypecheckedFile {
        functions,
        top_level_statements,
        module,
    } = module;

    let mut module_functions = Vec::with_capacity(functions.len());
    for func in functions {
        let func_ty = &declarations.id_to_func[&func.id];
        if func_ty.is_coroutine {
            module_functions.extend(lower_coroutine(declarations, ast, module, func, func_ty));
        } else {
            module_functions.push(lower_function(declarations, ast, func, func_ty));
        }
    }

    let top_level_ty = top_level_statements
        .last()
        .and_then(|last| last.ty.get().cloned());
    HirModule {
        functions: module_functions,
        top_level_statements: HirNode::autogenerated(
            HirNodeValue::Sequence(
                top_level_statements
                    .into_iter()
                    .map(|stmt| lower_node(declarations, ast, stmt))
                    .collect(),
            ),
            top_level_ty.unwrap_or(ExpressionType::Void),
        ),
    }
}

fn lower_coroutine(
    decls: &DeclarationContext,
    ast: &AstArena,
    module: &FileDeclarations,
    func: TypecheckedFunction<'_>,
    func_ty: &FuncType,
) -> [HirFunction; 2] {
    // TODO: insert this generator function into the declaration context
    let generator_function_id = module.new_func_id();

    let coroutine_start = HirFunction {
        id: func.id,
        name: Some(func.name),
        body: HirNode::autogenerated(
            HirNodeValue::Sequence(vec![HirNode::autogenerated(
                HirNodeValue::GeneratorCreate {
                    generator_function: generator_function_id,
                    // TODO: pass the parameters down
                    args: vec![],
                },
                func_ty.returns.clone(),
            )]),
            func_ty.returns.clone(),
        ),
        generator: None,
    };

    // TODO: pass generator into generator function body
    let mut body = lower_node(decls, ast, ast.get(func.func.body));
    let HirNodeValue::Sequence(instrs) = &mut body.value else {
        unreachable!()
    };
    let generator_var_id = VariableID::new();
    let generator_ty =
        ExpressionType::Pointer(PointerKind::Unique, Box::new(func_ty.returns.clone()));
    let mut body_instrs = vec![HirNode::autogenerated(
        HirNodeValue::Parameter(0, generator_var_id),
        generator_ty.clone(),
    )];
    let ExpressionType::Generator { param_ty, .. } = &func_ty.returns else {
        unreachable!()
    };
    let mut param_types = vec![generator_ty.clone()];
    let param_var_id = if param_ty.as_ref() != &ExpressionType::Void {
        let var_id = VariableID::new();
        body_instrs.push(HirNode::autogenerated(
            HirNodeValue::Parameter(1, var_id),
            param_ty.as_ref().clone(),
        ));
        param_types.push(param_ty.as_ref().clone());
        Some(var_id)
    } else {
        None
    };
    body_instrs.push(HirNode::autogenerated(
        HirNodeValue::GeneratorResume(Box::new(HirNode::autogenerated(
            HirNodeValue::Access(
                Box::new(HirNode::autogenerated(
                    HirNodeValue::VariableReference(generator_var_id.into()),
                    generator_ty.clone(),
                )),
                "resume_point".to_string(),
            ),
            ExpressionType::Primitive(PrimitiveType::PointerSize),
        ))),
        ExpressionType::Void,
    ));
    body_instrs.push(HirNode::autogenerated(
        HirNodeValue::GotoLabel(0),
        ExpressionType::Void,
    ));
    body_instrs.append(instrs);
    body.value = HirNodeValue::Sequence(body_instrs);

    let coroutine_body = HirFunction {
        id: generator_function_id,
        name: None,
        body,
        generator: Some(GeneratorProperties {
            generator_var_id,
            param_var_id,
            ty: generator_ty.clone(),
        }),
    };

    [coroutine_start, coroutine_body]
}

fn lower_function(
    decls: &DeclarationContext,
    ast: &AstArena,
    func: TypecheckedFunction<'_>,
    func_ty: &FuncType,
) -> HirFunction {
    let mut instructions: Vec<_> = func
        .func
        .params
        .iter()
        .enumerate()
        .map(|(i, (id, _param))| {
            HirNode::autogenerated(HirNodeValue::Parameter(i, *id), func_ty.params[i].clone())
        })
        .collect();
    let mut body = lower_node(decls, ast, ast.get(func.func.body));
    let HirNodeValue::Sequence(instrs) = &mut body.value else {
        unreachable!()
    };
    instructions.append(instrs);
    std::mem::swap(&mut instructions, instrs);
    HirFunction {
        id: func.id,
        name: Some(func.name),
        body,
        generator: None,
    }
}

pub fn lower_node(decls: &DeclarationContext, ast: &AstArena, node: &AstNode) -> HirNode {
    let value = match &node.value {
        AstNodeValue::Int(x) => HirNodeValue::Int(*x),
        AstNodeValue::Float(x) => HirNodeValue::Float(*x),
        AstNodeValue::Bool(x) => HirNodeValue::Bool(*x),
        AstNodeValue::Null => HirNodeValue::Null,
        AstNodeValue::CharLiteral(x) => HirNodeValue::CharLiteral(*x),
        AstNodeValue::StringLiteral(x) => HirNodeValue::StringLiteral(x.clone()),

        AstNodeValue::While(cond, body) => {
            // TODO: can you assign out of a while?
            let cond = lower_node_alloc(decls, ast, ast.get(*cond));
            let body = lower_node_alloc(decls, ast, ast.get(*body));
            HirNodeValue::While(cond, body)
        }
        AstNodeValue::Loop(body) => {
            HirNodeValue::Loop(lower_node_alloc(decls, ast, ast.get(*body)))
        }
        AstNodeValue::Block(contents) => {
            let contents = contents
                .iter()
                .map(|node| lower_node(decls, ast, node))
                .collect();

            HirNodeValue::Sequence(contents)
        }
        // TODO
        AstNodeValue::If(IfDeclaration {
            condition,
            if_branch,
            else_branch,
        }) => {
            let condition = lower_node_alloc(decls, ast, ast.get(*condition));
            let if_branch = lower_node_alloc(decls, ast, ast.get(*if_branch));
            let else_branch = else_branch
                .as_ref()
                .map(|else_branch| lower_node_alloc(decls, ast, ast.get(*else_branch)));

            HirNodeValue::If(condition, if_branch, else_branch)
        }

        AstNodeValue::TakeUnique(inner) => {
            HirNodeValue::TakeUnique(lower_node_alloc(decls, ast, ast.get(*inner)))
        }
        AstNodeValue::TakeRef(inner) => {
            HirNodeValue::TakeShared(lower_node_alloc(decls, ast, ast.get(*inner)))
        }
        AstNodeValue::Deref(inner) => {
            HirNodeValue::Dereference(lower_node_alloc(decls, ast, ast.get(*inner)))
        }

        // Statement doesn't actually add a node - the inner expression
        // has what really counts
        AstNodeValue::Statement(inner) => return lower_node(decls, ast, ast.get(*inner)),

        AstNodeValue::Declaration(_lvalue, type_hint, rvalue, variable_id) => {
            let ty = type_hint
                .as_ref()
                .map(|node| ast.get(*node).ty.get().unwrap().clone())
                .unwrap_or_else(|| ast.get(*rvalue).ty.get().unwrap().clone());
            let lvalue = Box::new(HirNode {
                id: NodeID::new(),
                value: HirNodeValue::VariableReference((*variable_id).into()),
                ty: ty.clone(),
                provenance: Some(node.provenance.clone()),
            });
            let rvalue = lower_node_alloc(decls, ast, ast.get(*rvalue));
            let statements = vec![
                HirNode::from_ast(node, HirNodeValue::Declaration(*variable_id), ty),
                HirNode {
                    id: NodeID::new(),
                    value: HirNodeValue::Assignment(lvalue, rvalue),
                    ty: ExpressionType::Void,
                    provenance: Some(node.provenance.clone()),
                },
            ];
            HirNodeValue::Sequence(statements)
        }
        AstNodeValue::Name { referenced_id, .. } => HirNodeValue::VariableReference(
            *referenced_id.get().expect("referenced ID to be filled in"),
        ),

        AstNodeValue::Return(inner) => HirNodeValue::Return(
            inner
                .as_ref()
                .map(|inner| lower_node_alloc(decls, ast, ast.get(*inner))),
        ),
        AstNodeValue::Yield(inner) => HirNodeValue::Yield(
            inner
                .as_ref()
                .map(|inner| lower_node_alloc(decls, ast, ast.get(*inner))),
        ),
        AstNodeValue::BinExpr(BinOp::Dot, left, right) => {
            let left = ast.get(*left);
            let right = ast.get(*right);
            let expr_ty = fully_dereference(left.ty.get().unwrap());
            let AstNodeValue::Name { value: name, .. } = &right.value else {
                unreachable!()
            };
            if let Some(TypeDeclaration::Module(module)) =
                expr_ty.type_id().and_then(|id| decls.id_to_decl.get(id))
            {
                let export = &module.exports[name];
                HirNodeValue::VariableReference(match export {
                    ExpressionType::InstanceOf(id) | ExpressionType::ReferenceToType(id) => {
                        (*id).into()
                    }
                    ExpressionType::ReferenceToFunction(id) => (*id).into(),
                    _ => todo!("non-type, non-function module exports"),
                })
            } else {
                let left = lower_node_alloc(decls, ast, left);
                HirNodeValue::Access(left, name.clone())
            }
        }
        AstNodeValue::BinExpr(BinOp::NullChaining, left, right) => {
            let left = ast.get(*left);
            let right = ast.get(*right);

            let left = lower_node_alloc(decls, ast, left);
            let mut name_list = Vec::new();
            traverse_dots(ast, right, |name, _| {
                name_list.push(name.to_string());
            });
            HirNodeValue::NullableTraverse(left, name_list)
        }
        AstNodeValue::BinExpr(BinOp::Index, left, right) => {
            let left = ast.get(*left);
            let right = ast.get(*right);

            let ty = left.ty.get().unwrap();
            let left = lower_node_alloc(decls, ast, left);
            let right = lower_node_alloc(decls, ast, right);
            match ty {
                ExpressionType::Collection(collection) => match collection {
                    CollectionType::Dict(_, _) => HirNodeValue::DictIndex(left, right),
                    CollectionType::Array(_) => HirNodeValue::ArrayIndex(left, right),
                    CollectionType::String => todo!(),
                    CollectionType::ReferenceCounter(_) => unreachable!(),
                    CollectionType::Cell(_) => unreachable!(),
                },
                _ => unreachable!(),
            }
        }
        AstNodeValue::UnaryExpr(op, child) => {
            let child = lower_node_alloc(decls, ast, ast.get(*child));
            HirNodeValue::UnaryLogical(
                match op {
                    UnaryOp::BooleanNot => UnaryLogicalOp::BooleanNot,
                },
                child,
            )
        }
        AstNodeValue::BinExpr(op, left, right) => {
            let left = lower_node_alloc(decls, ast, ast.get(*left));
            let right = lower_node_alloc(decls, ast, ast.get(*right));

            match op {
                BinOp::AddAssign => HirNodeValue::Assignment(
                    left.clone(),
                    Box::new(HirNode::from_ast_void(
                        node,
                        HirNodeValue::Arithmetic(ArithmeticOp::Add, left, right),
                    )),
                ),
                BinOp::SubtractAssign => HirNodeValue::Assignment(
                    left.clone(),
                    Box::new(HirNode::from_ast_void(
                        node,
                        HirNodeValue::Arithmetic(ArithmeticOp::Subtract, left, right),
                    )),
                ),
                BinOp::MultiplyAssign => HirNodeValue::Assignment(
                    left.clone(),
                    Box::new(HirNode::from_ast_void(
                        node,
                        HirNodeValue::Arithmetic(ArithmeticOp::Multiply, left, right),
                    )),
                ),
                BinOp::DivideAssign => HirNodeValue::Assignment(
                    left.clone(),
                    Box::new(HirNode::from_ast_void(
                        node,
                        HirNodeValue::Arithmetic(ArithmeticOp::Divide, left, right),
                    )),
                ),
                BinOp::Assignment => HirNodeValue::Assignment(left, right),

                BinOp::Add => HirNodeValue::Arithmetic(ArithmeticOp::Add, left, right),
                BinOp::Subtract => HirNodeValue::Arithmetic(ArithmeticOp::Subtract, left, right),
                BinOp::Multiply => HirNodeValue::Arithmetic(ArithmeticOp::Multiply, left, right),
                BinOp::Divide => HirNodeValue::Arithmetic(ArithmeticOp::Divide, left, right),
                BinOp::LessThan => HirNodeValue::Comparison(ComparisonOp::LessThan, left, right),
                BinOp::GreaterThan => {
                    HirNodeValue::Comparison(ComparisonOp::GreaterThan, left, right)
                }
                BinOp::LessEqualThan => {
                    HirNodeValue::Comparison(ComparisonOp::LessEqualThan, left, right)
                }
                BinOp::GreaterEqualThan => {
                    HirNodeValue::Comparison(ComparisonOp::GreaterEqualThan, left, right)
                }
                BinOp::EqualTo => HirNodeValue::Comparison(ComparisonOp::EqualTo, left, right),
                BinOp::NotEquals => HirNodeValue::Comparison(ComparisonOp::NotEquals, left, right),
                BinOp::BooleanAnd => {
                    HirNodeValue::BinaryLogical(BinaryLogicalOp::BooleanAnd, left, right)
                }
                BinOp::BooleanOr => {
                    HirNodeValue::BinaryLogical(BinaryLogicalOp::BooleanOr, left, right)
                }
                BinOp::NullCoalesce => HirNodeValue::NullCoalesce(left, right),
                BinOp::Index | BinOp::Dot | BinOp::NullChaining => unreachable!(),
                BinOp::Concat => HirNodeValue::StringConcat(left, right),
            }
        }
        AstNodeValue::Call(func, params) => {
            let func = lower_node_alloc(decls, ast, ast.get(*func));
            let params = params
                .iter()
                .map(|param| lower_node(decls, ast, param))
                .collect();
            HirNodeValue::Call(func, params)
        }
        AstNodeValue::RecordLiteral { name, fields } => {
            let name = ast.get(*name);
            let AstNodeValue::Name { referenced_id, .. } = &name.value else {
                panic!("Struct literal must have Name");
            };
            let id = referenced_id
                .get()
                .expect("referenced fields to be filled in")
                .as_type();
            match &decls.id_to_decl[&id] {
                TypeDeclaration::Struct(_) => {
                    let fields = fields
                        .iter()
                        .map(|(name, field)| (name.clone(), lower_node(decls, ast, field)))
                        .collect();
                    HirNodeValue::StructLiteral(id, fields)
                }
                _ => unreachable!(),
            }
        }
        AstNodeValue::ReferenceCountLiteral(inner) => {
            HirNodeValue::ReferenceCountLiteral(lower_node_alloc(decls, ast, ast.get(*inner)))
        }
        AstNodeValue::CellLiteral(inner) => {
            HirNodeValue::CellLiteral(lower_node_alloc(decls, ast, ast.get(*inner)))
        }
        AstNodeValue::DictLiteral(elements) => HirNodeValue::DictLiteral(
            elements
                .iter()
                .map(|(key, value)| (lower_node(decls, ast, key), lower_node(decls, ast, value)))
                .collect(),
        ),
        AstNodeValue::ArrayLiteral(arr) => {
            let arr = arr
                .iter()
                .map(|elem| lower_node(decls, ast, elem))
                .collect();
            HirNodeValue::ArrayLiteral(arr)
        }
        AstNodeValue::ArrayLiteralLength(elem, count) => {
            let elem = lower_node_alloc(decls, ast, ast.get(*elem));
            let count = lower_node_alloc(decls, ast, ast.get(*count));
            HirNodeValue::ArrayLiteralLength(elem, count)
        }

        AstNodeValue::Match(match_decl) => {
            let match_inner = ast.get(match_decl.value);
            let union_node = lower_node_alloc(decls, ast, match_inner);
            let mut temp_variable_declaration = None;
            let union_node = if union_node.is_valid_lvalue() {
                union_node
            } else {
                let union_temp_id = VariableID::new();
                let ty = union_node.ty.clone();
                let var_reference = HirNode::from_ast(
                    match_inner,
                    HirNodeValue::VariableReference(union_temp_id.into()),
                    ty.clone(),
                );
                temp_variable_declaration = Some(HirNodeValue::Sequence(vec![
                    HirNode::from_ast(
                        match_inner,
                        HirNodeValue::Declaration(union_temp_id),
                        ty.clone(),
                    ),
                    HirNode::from_ast(
                        match_inner,
                        HirNodeValue::Assignment(Box::new(var_reference.clone()), union_node),
                        ExpressionType::Void,
                    ),
                ]));

                Box::new(var_reference)
            };
            let match_decl_ty = match_inner.ty.get().unwrap();
            let value = Box::new(HirNode::autogenerated(
                HirNodeValue::UnionTag(union_node.clone()),
                ExpressionType::Primitive(PrimitiveType::PointerSize),
            ));
            let Some(TypeDeclaration::Union(union_decl)) = shallow_dereference(match_decl_ty)
                .type_id()
                .map(|id| &decls.id_to_decl[id])
            else {
                unreachable!()
            };
            let cases = union_decl
                .variant_order
                .iter()
                .map(|union_variant| {
                    let matching_variant = match_decl.cases.iter().find_map(|case| {
                        case.variants
                            .iter()
                            .find(|match_variant| &match_variant.name == union_variant)
                            .map(|match_variant| (case, match_variant))
                    });
                    // If this case isn't used in this match statement, skip it
                    let Some((case_decl, _variant_decl)) = matching_variant else {
                        return HirNode::autogenerated(
                            HirNodeValue::Sequence(Vec::new()),
                            ExpressionType::Void,
                        );
                    };
                    let variant_ty = &union_decl.variants[union_variant];
                    let body = lower_node(decls, ast, &case_decl.body);
                    let body_ty = body.ty.clone();
                    // If there's no variable to bind, return just the body
                    let Some(variant_ty) = variant_ty else {
                        return HirNode::autogenerated(
                            HirNodeValue::Sequence(vec![body]),
                            body_ty.clone(),
                        );
                    };
                    let seq = vec![
                        // Declare the binding variable
                        HirNode::autogenerated(
                            HirNodeValue::Declaration(case_decl.var_id),
                            variant_ty.clone(),
                        ),
                        // Assign either the value or a reference to the value
                        HirNode::autogenerated(
                            HirNodeValue::Assignment(
                                Box::new(HirNode::autogenerated(
                                    HirNodeValue::VariableReference(case_decl.var_id.into()),
                                    variant_ty.clone(),
                                )),
                                // If the union passed to the case statement is a pointer, then
                                // the binding inside the case statement should also be a pointer
                                if let ExpressionType::Pointer(ptr_ty, _) = match_decl_ty {
                                    let variant_ty = ExpressionType::Pointer(
                                        *ptr_ty,
                                        Box::new(variant_ty.clone()),
                                    );
                                    let union_variant_node = Box::new(HirNode::autogenerated(
                                        HirNodeValue::UnionVariant(
                                            Box::new(HirNode::autogenerated(
                                                HirNodeValue::Dereference(union_node.clone()),
                                                shallow_dereference(match_decl_ty).clone(),
                                            )),
                                            union_variant.clone(),
                                        ),
                                        variant_ty.clone(),
                                    ));
                                    Box::new(HirNode::autogenerated(
                                        match ptr_ty {
                                            PointerKind::Shared => {
                                                HirNodeValue::TakeShared(union_variant_node)
                                            }
                                            PointerKind::Unique => {
                                                HirNodeValue::TakeUnique(union_variant_node)
                                            }
                                        },
                                        variant_ty.clone(),
                                    ))
                                } else {
                                    Box::new(HirNode::autogenerated(
                                        HirNodeValue::UnionVariant(
                                            union_node.clone(),
                                            union_variant.clone(),
                                        ),
                                        variant_ty.clone(),
                                    ))
                                },
                            ),
                            ExpressionType::Void,
                        ),
                        body,
                    ];
                    HirNode::autogenerated(HirNodeValue::Sequence(seq), body_ty.clone())
                })
                .collect();

            if let Some(mut seq) = temp_variable_declaration {
                let HirNodeValue::Sequence(body) = &mut seq else {
                    unreachable!()
                };
                body.push(HirNode::from_ast(
                    node,
                    HirNodeValue::Switch { value, cases },
                    node.ty.get().unwrap().clone(),
                ));
                seq
            } else {
                HirNodeValue::Switch { value, cases }
            }
        }
        AstNodeValue::BorrowDeclaration(_name, value, variable_id) => {
            let rvalue = lower_node_alloc(decls, ast, ast.get(*value));
            let lvalue = Box::new(HirNode {
                id: NodeID::new(),
                value: HirNodeValue::VariableReference((*variable_id).into()),
                ty: rvalue.ty.clone(),
                provenance: Some(node.provenance.clone()),
            });
            let statements = vec![
                HirNode::from_ast(
                    node,
                    HirNodeValue::Declaration(*variable_id),
                    rvalue.ty.clone(),
                ),
                HirNode {
                    id: NodeID::new(),
                    value: HirNodeValue::Assignment(lvalue, rvalue),
                    ty: ExpressionType::Void,
                    provenance: Some(node.provenance.clone()),
                },
            ];
            HirNodeValue::Sequence(statements)
        }

        // Essentially strip constant declarations out when lowering
        AstNodeValue::ConstDeclaration { .. } => HirNodeValue::Sequence(vec![]),

        AstNodeValue::FunctionDeclaration(_)
        | AstNodeValue::ExternFunctionBinding(_)
        | AstNodeValue::StructDeclaration(_)
        | AstNodeValue::UnionDeclaration(_)
        | AstNodeValue::InterfaceDeclaration(_)
        | AstNodeValue::Import(_)
        | AstNodeValue::UniqueType(_)
        | AstNodeValue::VoidType
        | AstNodeValue::SharedType(_)
        | AstNodeValue::NullableType(_)
        | AstNodeValue::RequiredFunction(_)
        | AstNodeValue::DictType(_, _)
        | AstNodeValue::ArrayType(_)
        | AstNodeValue::CellType(_)
        | AstNodeValue::RcType(_)
        | AstNodeValue::GeneratorType { .. } => unreachable!("Can't have these in a function body"),
    };

    HirNode::from_ast(node, value, node.ty.get().expect("type filled").clone())
}

fn lower_node_alloc(decls: &DeclarationContext, ast: &AstArena, node: &AstNode) -> Box<HirNode> {
    Box::new(lower_node(decls, ast, node))
}
