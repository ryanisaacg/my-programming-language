use std::collections::{HashMap, HashSet};

use crate::{
    declaration_context::{resolve_type_expr, DeclarationContext, FileDeclarations, TypeID},
    id::{AnyID, FunctionID, VariableID},
    multi_error::{merge_results, merge_results_or_value},
    parser::{
        AstArena, AstNode, AstNodeValue, BinOp, FunctionDeclarationValue, IfDeclaration,
        InterfaceDeclarationValue, MatchDeclaration, ParsedFile, StructDeclarationValue, UnaryOp,
    },
    provenance::SourceRange,
};

mod error;
pub use error::TypecheckError;
mod types;
pub use types::*;

pub struct TypecheckContext<'ast, 'decl> {
    ast: &'ast AstArena,
    declarations: &'decl DeclarationContext,
    top_level_type_names: HashMap<&'decl str, TypeID>,
    top_level_function_names: HashMap<&'decl str, FunctionID>,
    top_level_name_to_expr_type: HashMap<String, (AnyID, ExpressionType)>,
}

impl<'ast, 'decl> TypecheckContext<'ast, 'decl> {
    pub fn new(
        file: &'ast ParsedFile,
        current_module_name: &str,
        declarations: &'decl DeclarationContext,
    ) -> Result<Self, TypecheckError> {
        let mut top_level_type_names = HashMap::new();
        let mut top_level_function_names = HashMap::new();
        let mut top_level_name_to_expr_type = HashMap::new();

        let module = &declarations.files[current_module_name];

        // Insert the module's top-level
        let top_level = declarations.id_to_decl[&module.module_id]
            .as_module()
            .unwrap();
        for (name, (const_id, ty)) in top_level.exports.iter() {
            if const_id.is_some() {
                continue;
            }
            match ty {
                ExpressionType::Void
                | ExpressionType::Unreachable
                | ExpressionType::Primitive(_)
                | ExpressionType::Pointer(_, _)
                | ExpressionType::Collection(_)
                | ExpressionType::Null
                | ExpressionType::Nullable(_)
                | ExpressionType::TypeParameterReference(_)
                | ExpressionType::Generator { .. }
                | ExpressionType::FunctionReference { .. }
                | ExpressionType::InstanceOf(_) => unreachable!(),
                ExpressionType::ReferenceToType(ty_id) => {
                    let value = &declarations.id_to_decl[ty_id];
                    top_level_type_names.insert(name.as_str(), *ty_id);
                    top_level_name_to_expr_type
                        .insert(name.clone(), (value.fn_id_or_type_id(), value.ref_to()));
                }
                ExpressionType::ReferenceToFunction(fn_id) => {
                    let value = &declarations.id_to_func[fn_id];
                    top_level_function_names.insert(name.as_str(), *fn_id);
                    top_level_name_to_expr_type.insert(
                        name.clone(),
                        (
                            value.id.into(),
                            ExpressionType::ReferenceToFunction(value.id),
                        ),
                    );
                }
            }
        }

        // Insert all the imports
        for (name, (const_id, expr)) in module.imports() {
            if let Some(const_id) = const_id {
                let const_id = *const_id;
                top_level_name_to_expr_type
                    .insert(name.to_string(), (const_id.into(), expr.clone()));
            } else {
                match expr {
                    ExpressionType::ReferenceToFunction(fn_id) => {
                        let fn_id = *fn_id;
                        top_level_function_names.insert(name, fn_id);
                        top_level_name_to_expr_type.insert(
                            name.to_string(),
                            (fn_id.into(), ExpressionType::ReferenceToFunction(fn_id)),
                        );
                    }
                    ExpressionType::ReferenceToType(ty_id) => {
                        let ty_id = *ty_id;
                        top_level_type_names.insert(name, ty_id);
                        top_level_name_to_expr_type.insert(
                            name.to_string(),
                            (ty_id.into(), ExpressionType::ReferenceToType(ty_id)),
                        );
                    }
                    _ => unreachable!(),
                }
            }
        }

        let mut context = TypecheckContext {
            ast: &file.arena,
            declarations,
            top_level_name_to_expr_type,
            top_level_function_names,
            top_level_type_names,
        };

        // Insert all the constants
        let mut constants = HashMap::new();
        for statement in file.top_level.iter() {
            if let AstNodeValue::ConstDeclaration { .. } = &statement.value {
                typecheck_const(&context, &[], &mut constants, statement)?;
            }
        }
        context.top_level_name_to_expr_type.extend(constants);

        Ok(context)
    }

    fn id_to_decl(&self) -> &HashMap<TypeID, TypeDeclaration> {
        &self.declarations.id_to_decl
    }

    fn decl(&self, id: &TypeID) -> Option<&TypeDeclaration> {
        self.declarations.id_to_decl.get(id)
    }
}

pub struct TypecheckedFile<'ast, 'decl> {
    pub functions: Vec<TypecheckedFunction<'ast>>,
    pub top_level_statements: Vec<&'ast AstNode>,
    pub module: &'decl FileDeclarations,
}

#[derive(Clone, Debug)]
pub struct TypecheckedFunction<'ast> {
    pub id: FunctionID,
    pub name: String,
    pub func: &'ast FunctionDeclarationValue,
    pub self_var_id: Option<VariableID>,
}

pub fn typecheck<'ast, 'decl>(
    file: &'ast ParsedFile,
    current_module_name: &str,
    declarations: &'decl DeclarationContext,
) -> Result<TypecheckedFile<'ast, 'decl>, TypecheckError> {
    let context = TypecheckContext::new(file, current_module_name, declarations)?;
    let module = &declarations.files[current_module_name];

    let mut functions = Vec::new();
    let mut top_level_statements = Vec::new();
    let mut top_level_scope = HashMap::new();
    let mut results = Ok(());

    for statement in file.top_level.iter() {
        merge_results(
            &mut results,
            typecheck_node(
                statement,
                &context,
                &mut top_level_scope,
                &mut functions,
                &mut top_level_statements,
            ),
        );
    }
    results?;

    Ok(TypecheckedFile {
        functions,
        top_level_statements,
        module,
    })
}

pub fn typecheck_node<'a>(
    statement: &'a AstNode,
    context: &TypecheckContext<'a, '_>,
    top_level_scope: &mut HashMap<String, (AnyID, ExpressionType)>,
    functions: &mut Vec<TypecheckedFunction<'a>>,
    top_level_statements: &mut Vec<&'a AstNode>,
) -> Result<(), TypecheckError> {
    match &statement.value {
        AstNodeValue::FunctionDeclaration(func) => {
            let func_id = &context.top_level_function_names[func.name.as_str()];
            let func_type = &context.declarations.id_to_func[func_id];
            functions.push(typecheck_function(context, func, func_type)?);
        }
        AstNodeValue::StructDeclaration(StructDeclarationValue {
            name,
            associated_functions,
            ..
        })
        | AstNodeValue::InterfaceDeclaration(InterfaceDeclarationValue {
            name,
            associated_functions,
            ..
        }) => {
            let ty_id = &context.top_level_type_names[name.as_str()];
            let (TypeDeclaration::Struct(StructType {
                associated_functions: associated_functions_ty,
                ..
            })
            | TypeDeclaration::Interface(InterfaceType {
                associated_functions: associated_functions_ty,
                ..
            })) = &context.declarations.id_to_decl[ty_id]
            else {
                unreachable!()
            };
            let associated_functions = associated_functions
                .iter()
                .filter_map(|function| {
                    let function = context.ast.get(*function);
                    let AstNodeValue::FunctionDeclaration(func) = &function.value else {
                        return None;
                    };
                    let func_id = &associated_functions_ty[func.name.as_str()];
                    let func_ty = &context.declarations.id_to_func[func_id];
                    Some(typecheck_function(context, func, func_ty))
                })
                .collect::<Result<Vec<_>, _>>()?;
            functions.extend(associated_functions.iter().cloned());
        }
        // These nodes don't execute anything and therefore don't need to be typechecked
        AstNodeValue::Import(_)
        | AstNodeValue::UnionDeclaration(_)
        | AstNodeValue::ExternFunctionBinding(_) => {}
        // Constants are extracted and type-checked earlier in the process, but still need
        // to be present so we can locate them in the HIR
        AstNodeValue::ConstDeclaration { .. } => {
            top_level_statements.push(statement);
        }
        _ => {
            typecheck_expression(
                statement,
                &[&context.top_level_name_to_expr_type],
                top_level_scope,
                context,
                None,
            )?;
            top_level_statements.push(statement);
        }
    }

    Ok(())
}

fn typecheck_function<'a>(
    context: &TypecheckContext,
    function: &'a FunctionDeclarationValue,
    function_type: &FuncType,
) -> Result<TypecheckedFunction<'a>, TypecheckError> {
    let mut self_var_id = None;

    let parameters = if function.self_param.is_some() {
        let mut parameter_context_entries = HashMap::new();
        let self_var = VariableID::new();
        self_var_id = Some(self_var);
        let self_ty = function_type.params[0].clone();
        parameter_context_entries.insert("self".to_string(), (self_var.into(), self_ty));

        parameter_context_entries.extend(
            function
                .params
                .iter()
                .zip(function_type.params.iter().skip(1))
                .map(|((id, name), param)| (name.name.clone(), ((*id).into(), param.clone()))),
        );

        parameter_context_entries
    } else {
        function
            .params
            .iter()
            .zip(function_type.params.iter())
            .map(|((id, name), param)| (name.name.clone(), ((*id).into(), param.clone())))
            .collect()
    };

    let body = context.ast.get(function.body);
    if function.is_coroutine {
        let ExpressionType::Generator { param_ty, .. } = &function_type.returns else {
            return Err(TypecheckError::MustReturnGenerator(
                function_type.provenance.clone().unwrap(),
            ));
        };
        let return_ty = typecheck_expression(
            body,
            &[&context.top_level_name_to_expr_type, &parameters],
            &mut HashMap::new(),
            context,
            Some(param_ty),
        )?;

        if return_ty != &ExpressionType::Void && return_ty != &ExpressionType::Unreachable {
            return Err(TypecheckError::TypeMismatch {
                provenance: body.provenance.clone(),
                expected: ExpressionType::Void,
                received: return_ty.clone(),
            });
        }
    } else {
        let return_ty = typecheck_expression(
            body,
            &[&context.top_level_name_to_expr_type, &parameters],
            &mut HashMap::new(),
            context,
            None,
        )?;
        assert_assignable_to(
            context.declarations,
            &body.provenance,
            &function_type.returns,
            return_ty,
        )?;

        typecheck_returns(context, &function_type.returns, body)?;
    }

    Ok(TypecheckedFunction {
        id: function_type.id,
        name: function.name.clone(),
        func: function,
        self_var_id,
    })
}

fn typecheck_expression<'a>(
    node: &'a AstNode,
    outer_scopes: &[&HashMap<String, (AnyID, ExpressionType)>],
    current_scope: &mut HashMap<String, (AnyID, ExpressionType)>,
    context: &TypecheckContext,
    generator_input_ty: Option<&ExpressionType>,
) -> Result<&'a ExpressionType, TypecheckError> {
    let ty = match &node.value {
        AstNodeValue::FunctionDeclaration(_)
        | AstNodeValue::ExternFunctionBinding(_)
        | AstNodeValue::RequiredFunction(_)
        | AstNodeValue::StructDeclaration(_)
        | AstNodeValue::UnionDeclaration(_)
        | AstNodeValue::InterfaceDeclaration(_)
        | AstNodeValue::Import(_) => {
            unimplemented!("Can't do this inside a function");
        }
        AstNodeValue::UniqueType(_)
        | AstNodeValue::VoidType
        | AstNodeValue::SharedType(_)
        | AstNodeValue::ArrayType(_)
        | AstNodeValue::CellType(_)
        | AstNodeValue::RcType(_)
        | AstNodeValue::DictType(_, _)
        | AstNodeValue::NullableType(_)
        | AstNodeValue::GeneratorType { .. } => {
            panic!("illegal type expression in function body");
        }
        AstNodeValue::Statement(inner) => {
            typecheck_expression(
                context.ast.get(*inner),
                outer_scopes,
                current_scope,
                context,
                generator_input_ty,
            )?;
            ExpressionType::Void
        }
        AstNodeValue::Declaration(name, type_hint, value, variable_id) => {
            let value_ty = typecheck_expression(
                context.ast.get(*value),
                outer_scopes,
                current_scope,
                context,
                generator_input_ty,
            )?;
            let mut result = Ok(());
            if matches!(value_ty, ExpressionType::Pointer(_, _)) {
                merge_results(
                    &mut result,
                    Err(TypecheckError::IllegalFirstClassReference(
                        node.provenance.clone(),
                    )),
                );
            }

            if let Some(type_hint) = type_hint {
                let type_hint = context.ast.get(*type_hint);
                let hint_ty =
                    resolve_type_expr(context.ast, &context.top_level_type_names, type_hint)?;
                if matches!(hint_ty, ExpressionType::Pointer(_, _)) {
                    merge_results(
                        &mut result,
                        Err(TypecheckError::IllegalFirstClassReference(
                            node.provenance.clone(),
                        )),
                    );
                }

                merge_results(
                    &mut result,
                    assert_assignable_to(
                        context.declarations,
                        &type_hint.provenance,
                        &hint_ty,
                        value_ty,
                    ),
                );
                type_hint.ty.set(hint_ty.clone()).unwrap();
                current_scope.insert(name.clone(), ((*variable_id).into(), hint_ty));
            } else {
                if value_ty == &ExpressionType::Null
                    || value_ty == &ExpressionType::Unreachable
                    || value_ty == &ExpressionType::Void
                {
                    return Err(TypecheckError::NoNullDeclarations(node.provenance.clone()));
                }
                current_scope.insert(name.clone(), ((*variable_id).into(), value_ty.clone()));
            }

            result?;

            ExpressionType::Void
        }
        AstNodeValue::BorrowDeclaration(name, value, variable_id) => {
            let value = context.ast.get(*value);
            let value_ty = typecheck_expression(
                value,
                outer_scopes,
                current_scope,
                context,
                generator_input_ty,
            )?;
            current_scope.insert(name.clone(), ((*variable_id).into(), value_ty.clone()));

            let mut results = Ok(());
            if let AstNodeValue::TakeRef(child) | AstNodeValue::TakeUnique(child) = &value.value {
                if !validate_lvalue(context, context.ast.get(*child)) {
                    merge_results(
                        &mut results,
                        Err(TypecheckError::IllegalNonLvalueBorrow(
                            node.provenance.clone(),
                        )),
                    );
                }
            } else {
                merge_results(
                    &mut results,
                    Err(TypecheckError::IllegalNonRefBorrow(node.provenance.clone())),
                );
            };
            results?;

            ExpressionType::Void
        }
        AstNodeValue::ConstDeclaration { .. } => {
            return typecheck_const(context, outer_scopes, current_scope, node);
        }
        AstNodeValue::Return(returned) => {
            if let Some(returned) = returned {
                typecheck_expression(
                    context.ast.get(*returned),
                    outer_scopes,
                    current_scope,
                    context,
                    generator_input_ty,
                )?;
            }

            ExpressionType::Unreachable
        }
        AstNodeValue::Yield(yielded) => {
            let Some(yield_ctx_ty) = generator_input_ty else {
                return Err(TypecheckError::CannotYield(node.provenance.clone()));
            };
            if let Some(yielded) = yielded {
                typecheck_expression(
                    context.ast.get(*yielded),
                    outer_scopes,
                    current_scope,
                    context,
                    Some(yield_ctx_ty),
                )?;
            }

            yield_ctx_ty.clone()
        }
        AstNodeValue::Name {
            value: name,
            referenced_id,
        } => {
            let (ref_id, expr) =
                resolve_name(name, current_scope, outer_scopes).ok_or_else(|| {
                    TypecheckError::NameNotFound(node.provenance.clone(), name.clone())
                })?;
            referenced_id
                .set(ref_id)
                .expect("each node should be visited once");
            expr
        }
        AstNodeValue::Int(constant) => {
            if i32::try_from(*constant).is_ok() {
                ExpressionType::Primitive(PrimitiveType::Int32)
            } else {
                ExpressionType::Primitive(PrimitiveType::Int64)
            }
        }
        AstNodeValue::Float(constant) => {
            const ACCEPTABLE_DIFF: f64 = 0.0000001;
            if (*constant as f32 as f64 - *constant).abs() <= ACCEPTABLE_DIFF {
                ExpressionType::Primitive(PrimitiveType::Float32)
            } else {
                ExpressionType::Primitive(PrimitiveType::Float64)
            }
        }
        AstNodeValue::StringLiteral(_) => ExpressionType::Collection(CollectionType::String),
        AstNodeValue::CharLiteral(_) => ExpressionType::Primitive(PrimitiveType::Char),
        AstNodeValue::Null => ExpressionType::Null,
        AstNodeValue::Bool(_) => ExpressionType::Primitive(PrimitiveType::Bool),
        AstNodeValue::BinExpr(BinOp::Dot, left, right) => {
            let left = context.ast.get(*left);
            let right = context.ast.get(*right);

            let left_ty = typecheck_expression(
                left,
                outer_scopes,
                current_scope,
                context,
                generator_input_ty,
            )?;
            let AstNodeValue::Name { value: name, .. } = &right.value else {
                return Err(TypecheckError::IllegalDotRHS(right.provenance.clone()));
            };
            match fully_dereference(left_ty) {
                ExpressionType::InstanceOf(id) => context
                    .decl(id)
                    .unwrap()
                    .field_access(name, &right.provenance)?,
                ExpressionType::ReferenceToType(id) => match context.decl(id).unwrap() {
                    TypeDeclaration::Union(union_ty) => {
                        let variant_ty = union_ty.variants.get(name).ok_or_else(|| {
                            TypecheckError::FieldNotPresent(name.clone(), node.provenance.clone())
                        })?;

                        if let Some(variant_ty) = variant_ty {
                            ExpressionType::FunctionReference {
                                parameters: vec![variant_ty.clone()],
                                returns: Box::new(ExpressionType::InstanceOf(*id)),
                            }
                        } else {
                            ExpressionType::InstanceOf(*id)
                        }
                    }
                    TypeDeclaration::Module(module) => module
                        .exports
                        .get(name)
                        .map(|(_, ty)| ty)
                        .ok_or_else(|| {
                            TypecheckError::ExportNotFound(
                                node.provenance.clone(),
                                name.to_string(),
                            )
                        })?
                        .clone(),
                    // TODO: static functions on structs?
                    TypeDeclaration::Struct(_) | TypeDeclaration::Interface(_) => {
                        return Err(TypecheckError::IllegalDotLHS(left.provenance.clone()));
                    }
                },
                ExpressionType::Collection(CollectionType::Array(_item_ty)) => {
                    if let Some(ty) = context.declarations.array_intrinsics.get(name.as_str()) {
                        ExpressionType::ReferenceToFunction(ty.fn_id)
                    } else {
                        todo!("array methods")
                    }
                }
                ExpressionType::Collection(CollectionType::Dict(_key_ty, _value_ty)) => {
                    if let Some(ty) = context.declarations.dict_intrinsics.get(name.as_str()) {
                        ExpressionType::ReferenceToFunction(ty.fn_id)
                    } else {
                        todo!("dict methods")
                    }
                }
                ExpressionType::Collection(CollectionType::ReferenceCounter(_)) => {
                    if let Some(ty) = context.declarations.rc_intrinsics.get(name.as_str()) {
                        ExpressionType::ReferenceToFunction(ty.fn_id)
                    } else {
                        todo!("rc methods")
                    }
                }
                ExpressionType::Collection(CollectionType::Cell(_)) => {
                    if let Some(ty) = context.declarations.cell_intrinsics.get(name.as_str()) {
                        ExpressionType::ReferenceToFunction(ty.fn_id)
                    } else {
                        todo!("rc methods")
                    }
                }
                _ => {
                    return Err(TypecheckError::IllegalDotLHS(left.provenance.clone()));
                }
            }
        }
        AstNodeValue::BinExpr(BinOp::Concat, left, right) => {
            let left = context.ast.get(*left);
            let right = context.ast.get(*right);

            let mut results = Ok(());

            merge_results(
                &mut results,
                typecheck_expression(
                    left,
                    outer_scopes,
                    current_scope,
                    context,
                    generator_input_ty,
                )
                .and_then(|left_ty| {
                    assert_assignable_to(
                        context.declarations,
                        &left.provenance,
                        &ExpressionType::Collection(CollectionType::String),
                        left_ty,
                    )
                }),
            );
            merge_results(
                &mut results,
                typecheck_expression(
                    right,
                    outer_scopes,
                    current_scope,
                    context,
                    generator_input_ty,
                )
                .and_then(|right_ty| {
                    assert_assignable_to(
                        context.declarations,
                        &right.provenance,
                        &ExpressionType::Collection(CollectionType::String),
                        right_ty,
                    )
                }),
            );
            results?;

            ExpressionType::Collection(CollectionType::String)
        }
        AstNodeValue::BinExpr(BinOp::NullChaining, left, right) => {
            let left = context.ast.get(*left);
            let right = context.ast.get(*right);

            let left = typecheck_expression(
                left,
                outer_scopes,
                current_scope,
                context,
                generator_input_ty,
            )?;
            let ExpressionType::Nullable(ty) = fully_dereference(left) else {
                panic!("TODO: left side of nullable dot operator");
            };
            let mut field_ty = Ok(ty.as_ref().clone());
            traverse_dots(context.ast, right, |name, provenance| {
                let Ok(ty) = &field_ty else {
                    return;
                };
                let ExpressionType::InstanceOf(id) = ty else {
                    unreachable!()
                };
                field_ty = context.decl(id).unwrap().field_access(name, provenance);
            });
            ExpressionType::Nullable(Box::new(field_ty?))
        }
        AstNodeValue::BinExpr(BinOp::Index, collection, index) => {
            let collection = context.ast.get(*collection);
            let index = context.ast.get(*index);

            let collection_ty = typecheck_expression(
                collection,
                outer_scopes,
                current_scope,
                context,
                generator_input_ty,
            )?;
            match collection_ty {
                ExpressionType::Collection(CollectionType::Array(item_ty)) => {
                    let index_ty = typecheck_expression(
                        index,
                        outer_scopes,
                        current_scope,
                        context,
                        generator_input_ty,
                    )?;
                    assert_assignable_to(
                        context.declarations,
                        &index.provenance,
                        &ExpressionType::Primitive(PrimitiveType::PointerSize),
                        index_ty,
                    )?;

                    item_ty.as_ref().clone()
                }
                ExpressionType::Collection(CollectionType::Dict(key_ty, value_ty)) => {
                    let index_ty = typecheck_expression(
                        index,
                        outer_scopes,
                        current_scope,
                        context,
                        generator_input_ty,
                    )?;
                    assert_assignable_to(
                        context.declarations,
                        &index.provenance,
                        index_ty,
                        key_ty,
                    )?;

                    value_ty.as_ref().clone()
                }
                ty => todo!("can't index {:?}", ty),
            }
        }
        AstNodeValue::BinExpr(
            BinOp::Add | BinOp::Subtract | BinOp::Multiply | BinOp::Divide,
            left,
            right,
        ) => {
            let left = context.ast.get(*left);
            let right = context.ast.get(*right);

            let left = typecheck_expression(
                left,
                outer_scopes,
                current_scope,
                context,
                generator_input_ty,
            )?;
            let ExpressionType::Primitive(_) = fully_dereference(left) else {
                return Err(TypecheckError::ArithmeticMismatch(node.provenance.clone()));
            };
            let right = typecheck_expression(
                right,
                outer_scopes,
                current_scope,
                context,
                generator_input_ty,
            )?;
            let ExpressionType::Primitive(_) = fully_dereference(right) else {
                return Err(TypecheckError::ArithmeticMismatch(node.provenance.clone()));
            };
            if is_assignable_to(context.declarations, None, left, right) {
                left.clone()
            } else if is_assignable_to(context.declarations, None, right, left) {
                right.clone()
            } else {
                return Err(TypecheckError::ArithmeticMismatch(node.provenance.clone()));
            }
        }
        AstNodeValue::BinExpr(BinOp::BooleanAnd | BinOp::BooleanOr, left, right) => {
            let left = context.ast.get(*left);
            let right = context.ast.get(*right);

            let left_ty = typecheck_expression(
                left,
                outer_scopes,
                current_scope,
                context,
                generator_input_ty,
            )?;
            let right_ty = typecheck_expression(
                right,
                outer_scopes,
                current_scope,
                context,
                generator_input_ty,
            )?;

            let mut results = Ok(());
            merge_results(
                &mut results,
                assert_assignable_to(
                    context.declarations,
                    &left.provenance,
                    &ExpressionType::Primitive(PrimitiveType::Bool),
                    left_ty,
                ),
            );
            merge_results(
                &mut results,
                assert_assignable_to(
                    context.declarations,
                    &right.provenance,
                    &ExpressionType::Primitive(PrimitiveType::Bool),
                    right_ty,
                ),
            );
            results?;

            ExpressionType::Primitive(PrimitiveType::Bool)
        }
        AstNodeValue::BinExpr(
            BinOp::LessThan
            | BinOp::GreaterThan
            | BinOp::LessEqualThan
            | BinOp::GreaterEqualThan
            | BinOp::EqualTo
            | BinOp::NotEquals,
            left,
            right,
        ) => {
            let left = context.ast.get(*left);
            let right = context.ast.get(*right);

            let left = typecheck_expression(
                left,
                outer_scopes,
                current_scope,
                context,
                generator_input_ty,
            )?;
            let ExpressionType::Primitive(_) = fully_dereference(left) else {
                return Err(TypecheckError::ArithmeticMismatch(node.provenance.clone()));
            };
            let right = typecheck_expression(
                right,
                outer_scopes,
                current_scope,
                context,
                generator_input_ty,
            )?;
            let ExpressionType::Primitive(_) = fully_dereference(right) else {
                return Err(TypecheckError::ArithmeticMismatch(node.provenance.clone()));
            };

            ExpressionType::Primitive(PrimitiveType::Bool)
        }
        AstNodeValue::BinExpr(BinOp::Assignment, left, right) => {
            let left = context.ast.get(*left);
            let right = context.ast.get(*right);

            let mut errors = Ok(());
            let left_ty = typecheck_expression(
                left,
                outer_scopes,
                current_scope,
                context,
                generator_input_ty,
            )?;
            merge_results(&mut errors, validate_assignment_lhs(context, left));
            merge_results(
                &mut errors,
                ensure_no_assignment_to_reference(left_ty, &left.provenance),
            );
            let right_ty = merge_results_or_value(
                &mut errors,
                typecheck_expression(
                    right,
                    outer_scopes,
                    current_scope,
                    context,
                    generator_input_ty,
                ),
            );

            if let Some(right_ty) = right_ty {
                merge_results(
                    &mut errors,
                    assert_assignable_to(
                        context.declarations,
                        &right.provenance,
                        left_ty,
                        right_ty,
                    ),
                );
            }

            errors?;

            ExpressionType::Void
        }
        AstNodeValue::BinExpr(
            BinOp::AddAssign | BinOp::SubtractAssign | BinOp::MultiplyAssign | BinOp::DivideAssign,
            left,
            right,
        ) => {
            let left = context.ast.get(*left);
            let right = context.ast.get(*right);

            let mut errors = Ok(());
            let left_ty = typecheck_expression(
                left,
                outer_scopes,
                current_scope,
                context,
                generator_input_ty,
            )?;
            merge_results(&mut errors, validate_assignment_lhs(context, left));
            merge_results(
                &mut errors,
                ensure_no_assignment_to_reference(left_ty, &left.provenance),
            );
            let right_ty = typecheck_expression(
                right,
                outer_scopes,
                current_scope,
                context,
                generator_input_ty,
            )?;

            if !matches!(fully_dereference(left_ty), ExpressionType::Primitive(_))
                || !matches!(fully_dereference(right_ty), ExpressionType::Primitive(_))
            {
                merge_results(
                    &mut errors,
                    Err(TypecheckError::ArithmeticMismatch(node.provenance.clone())),
                );
            }

            merge_results(
                &mut errors,
                assert_assignable_to(context.declarations, &right.provenance, left_ty, right_ty),
            );

            errors?;

            ExpressionType::Void
        }
        AstNodeValue::BinExpr(BinOp::NullCoalesce, left, right) => {
            let left = context.ast.get(*left);
            let right = context.ast.get(*right);

            let left_ty = typecheck_expression(
                left,
                outer_scopes,
                current_scope,
                context,
                generator_input_ty,
            )?;
            let right_ty = typecheck_expression(
                right,
                outer_scopes,
                current_scope,
                context,
                generator_input_ty,
            )?;
            let ExpressionType::Nullable(ty) = left_ty else {
                return Err(TypecheckError::ExpectedNullableLHS(left.provenance.clone()));
            };

            assert_assignable_to(context.declarations, &right.provenance, ty, right_ty)?;

            ty.as_ref().clone()
        }
        AstNodeValue::While(condition, body) => {
            let condition = context.ast.get(*condition);
            let body = context.ast.get(*body);

            let condition_ty = typecheck_expression(
                condition,
                outer_scopes,
                current_scope,
                context,
                generator_input_ty,
            )?;
            let condition_deref = fully_dereference(condition_ty);
            if !matches!(
                condition_deref,
                ExpressionType::Primitive(PrimitiveType::Bool)
            ) {
                return Err(TypecheckError::TypeMismatch {
                    provenance: condition.provenance.clone(),
                    received: condition_ty.clone(),
                    expected: ExpressionType::Primitive(PrimitiveType::Bool),
                });
            }

            typecheck_expression(
                body,
                outer_scopes,
                current_scope,
                context,
                generator_input_ty,
            )?;

            ExpressionType::Void
        }
        AstNodeValue::Loop(body) => {
            typecheck_expression(
                context.ast.get(*body),
                outer_scopes,
                current_scope,
                context,
                generator_input_ty,
            )?;
            ExpressionType::Unreachable
        }
        AstNodeValue::If(IfDeclaration {
            condition,
            if_branch,
            else_branch,
        }) => {
            let condition = context.ast.get(*condition);
            let if_branch = context.ast.get(*if_branch);

            let condition_ty = typecheck_expression(
                condition,
                outer_scopes,
                current_scope,
                context,
                generator_input_ty,
            )?;
            let condition_deref = fully_dereference(condition_ty);
            if !matches!(
                condition_deref,
                ExpressionType::Primitive(PrimitiveType::Bool)
            ) {
                return Err(TypecheckError::TypeMismatch {
                    provenance: condition.provenance.clone(),
                    received: condition_ty.clone(),
                    expected: ExpressionType::Primitive(PrimitiveType::Bool),
                });
            }

            let if_ty = typecheck_expression(
                if_branch,
                outer_scopes,
                current_scope,
                context,
                generator_input_ty,
            )?;
            let else_ty = else_branch
                .as_ref()
                .map(|else_branch| {
                    typecheck_expression(
                        context.ast.get(*else_branch),
                        outer_scopes,
                        current_scope,
                        context,
                        generator_input_ty,
                    )
                })
                .transpose()?;

            match else_ty {
                Some(else_ty) => {
                    if is_assignable_to(context.declarations, None, if_ty, else_ty) {
                        if_ty.clone()
                    } else {
                        assert_assignable_to(
                            context.declarations,
                            &if_branch.provenance,
                            else_ty,
                            if_ty,
                        )?;
                        else_ty.clone()
                    }
                }
                None => ExpressionType::Void,
            }
        }
        AstNodeValue::Match(MatchDeclaration { value, cases }) => {
            let value = context.ast.get(*value);

            let input_ty = typecheck_expression(
                value,
                outer_scopes,
                current_scope,
                context,
                generator_input_ty,
            )?;
            let Some(TypeDeclaration::Union(union_ty)) = shallow_dereference(input_ty)
                .type_id()
                .map(|ty_id| &context.id_to_decl()[ty_id])
            else {
                return Err(TypecheckError::CaseStatementRequiresUnion(
                    value.provenance.clone(),
                ));
            };
            let mut return_type = None;
            let mut results = Ok(());
            let mut variants_matched_against = HashSet::new();
            for case in cases.iter() {
                let mut binding = BindingState::Uninit;
                for variant in case.variants.iter() {
                    variants_matched_against.insert(&variant.name);
                    let mut variant_ty = union_ty.variants[&variant.name].as_ref();
                    if !variant.bindings.is_empty() && variant.bindings[0] == "_" {
                        variant_ty = None;
                    }
                    variant.ty.set(variant_ty.cloned()).unwrap();
                    match &binding {
                        BindingState::Uninit => {
                            binding = match variant_ty {
                                Some(variant_ty) => {
                                    BindingState::Binding(&variant.bindings[0], variant_ty)
                                }
                                _ => BindingState::NoBinding,
                            };
                        }
                        BindingState::NoBinding => {
                            if variant_ty.is_some() {
                                merge_results(
                                    &mut results,
                                    Err(TypecheckError::BindingCountDoesntMatch(
                                        variant.provenance.clone(),
                                    )),
                                );
                            }
                        }
                        BindingState::Binding(name, binding_ty) => {
                            if let Some(variant_ty) = variant_ty {
                                merge_results(
                                    &mut results,
                                    assert_assignable_to(
                                        context.declarations,
                                        &node.provenance,
                                        binding_ty,
                                        variant_ty,
                                    ),
                                );
                                if *name != &variant.bindings[0] {
                                    merge_results(
                                        &mut results,
                                        Err(TypecheckError::BindingNameDoesntMatch(
                                            variant.provenance.clone(),
                                        )),
                                    );
                                }
                            } else {
                                merge_results(
                                    &mut results,
                                    Err(TypecheckError::BindingCountDoesntMatch(
                                        variant.provenance.clone(),
                                    )),
                                );
                            }
                        }
                    }
                }
                let mut scopes: Vec<&HashMap<_, _>> = Vec::with_capacity(outer_scopes.len() + 1);
                scopes.push(current_scope);
                scopes.extend_from_slice(outer_scopes);
                let mut child_scope = HashMap::new();
                if let BindingState::Binding(binding_name, binding_ty) = binding {
                    let binding_ty = if let ExpressionType::Pointer(ptr_ty, _) = input_ty {
                        ExpressionType::Pointer(*ptr_ty, Box::new(binding_ty.clone()))
                    } else {
                        binding_ty.clone()
                    };
                    child_scope.insert(binding_name.clone(), (case.var_id.into(), binding_ty));
                }

                let case_body = context.ast.get(case.body);
                let body_ty = typecheck_expression(
                    case_body,
                    &scopes,
                    &mut child_scope,
                    context,
                    generator_input_ty,
                )?;
                if let Some(return_type) = &return_type {
                    merge_results(
                        &mut results,
                        assert_assignable_to(
                            context.declarations,
                            &case_body.provenance,
                            return_type,
                            body_ty,
                        ),
                    );
                } else {
                    return_type = Some(body_ty.clone());
                }
            }
            if !union_ty
                .variants
                .keys()
                .all(|variant_name| variants_matched_against.contains(variant_name))
            {
                merge_results(
                    &mut results,
                    Err(TypecheckError::NonExhaustiveCase(node.provenance.clone())),
                );
            }

            let return_type = return_type.unwrap_or(ExpressionType::Void);
            if return_type.is_reference() {
                merge_results(
                    &mut results,
                    Err(TypecheckError::IllegalFirstClassReference(
                        node.provenance.clone(),
                    )),
                );
            }

            results?;

            return_type
        }
        AstNodeValue::Block(children) => {
            let mut scopes: Vec<&HashMap<_, _>> = Vec::with_capacity(outer_scopes.len() + 1);
            scopes.push(current_scope);
            scopes.extend_from_slice(outer_scopes);

            let mut result = Ok(());

            let mut child_scope = HashMap::new();
            let mut expr_ty = ExpressionType::Void;
            for (index, child) in children.iter().enumerate() {
                let child = context.ast.get(*child);
                let new_ty = typecheck_expression(
                    child,
                    &scopes[..],
                    &mut child_scope,
                    context,
                    generator_input_ty,
                )?;

                if expr_ty != ExpressionType::Unreachable {
                    expr_ty = new_ty.clone();
                }

                if index == children.len() - 1 {
                    if expr_ty.is_reference() {
                        merge_results(
                            &mut result,
                            Err(TypecheckError::IllegalFirstClassReference(
                                node.provenance.clone(),
                            )),
                        );
                    }
                } else if expr_ty != ExpressionType::Void && expr_ty != ExpressionType::Unreachable
                {
                    merge_results(
                        &mut result,
                        Err(TypecheckError::TypeMismatch {
                            provenance: child.provenance.clone(),
                            expected: ExpressionType::Void,
                            received: expr_ty.clone(),
                        }),
                    );
                }
            }
            expr_ty
        }
        AstNodeValue::Call(func, args) => {
            let func = context.ast.get(*func);
            match fully_dereference(typecheck_expression(
                func,
                outer_scopes,
                current_scope,
                context,
                generator_input_ty,
            )?) {
                ExpressionType::ReferenceToFunction(func_ty) => {
                    let func_ty = &context.declarations.id_to_func[func_ty];
                    let mut generic_args =
                        vec![ExpressionType::Unreachable; func_ty.type_param_count];

                    let params = if func_ty.is_associated {
                        if let AstNodeValue::BinExpr(BinOp::NullChaining | BinOp::Dot, lhs, _) =
                            &func.value
                        {
                            let lhs = context.ast.get(*lhs);
                            find_generic_bindings(
                                &mut generic_args[..],
                                &func_ty.params[0],
                                lhs.ty.get().expect("type info to be filled in"),
                            );
                        }

                        &func_ty.params[1..]
                    } else {
                        &func_ty.params[..]
                    };
                    if params.len() != args.len() {
                        return Err(TypecheckError::WrongArgsCount(node.provenance.clone()));
                    }

                    for (arg, param) in args.iter().zip(params.iter()) {
                        let arg = context.ast.get(*arg);
                        let arg_ty = typecheck_expression(
                            arg,
                            outer_scopes,
                            current_scope,
                            context,
                            generator_input_ty,
                        )?;
                        find_generic_bindings(&mut generic_args[..], param, arg_ty);
                        if !is_assignable_to(
                            context.declarations,
                            Some(&generic_args),
                            param,
                            arg_ty,
                        ) {
                            return Err(TypecheckError::TypeMismatch {
                                provenance: arg.provenance.clone(),
                                received: arg_ty.clone(),
                                expected: param.clone(),
                            });
                        }
                    }

                    let mut returns = func_ty.returns.clone();
                    returns.resolve_generics(&generic_args[..]);
                    returns
                }
                ExpressionType::Generator { yield_ty, param_ty } => {
                    // TODO: allow more than one parameter
                    if param_ty.as_ref() == &ExpressionType::Void {
                        if !args.is_empty() {
                            return Err(TypecheckError::WrongArgsCount(node.provenance.clone()));
                        }
                    } else {
                        if args.len() != 1 {
                            return Err(TypecheckError::WrongArgsCount(node.provenance.clone()));
                        }
                        let arg = args.last().unwrap();
                        let arg = context.ast.get(*arg);
                        let arg_ty = typecheck_expression(
                            arg,
                            outer_scopes,
                            current_scope,
                            context,
                            generator_input_ty,
                        )?;
                        // TODO: generic generators?
                        assert_assignable_to(
                            context.declarations,
                            &arg.provenance,
                            param_ty,
                            arg_ty,
                        )?;
                    }
                    yield_ty.as_ref().clone()
                }
                ExpressionType::FunctionReference {
                    parameters,
                    returns,
                } => {
                    let mut results = Ok(());
                    for (arg, param) in args.iter().zip(parameters.iter()) {
                        let arg = context.ast.get(*arg);
                        let arg_ty = typecheck_expression(
                            arg,
                            outer_scopes,
                            current_scope,
                            context,
                            generator_input_ty,
                        )?;
                        merge_results(
                            &mut results,
                            assert_assignable_to(
                                context.declarations,
                                &arg.provenance,
                                param,
                                arg_ty,
                            ),
                        );
                    }

                    results?;

                    returns.as_ref().clone()
                }
                _ => return Err(TypecheckError::CantCall(node.provenance.clone())),
            }
        }
        AstNodeValue::RecordLiteral { name, fields } => {
            let name = context.ast.get(*name);
            let ExpressionType::ReferenceToType(ty_id) = typecheck_expression(
                name,
                outer_scopes,
                current_scope,
                context,
                generator_input_ty,
            )?
            else {
                return Err(TypecheckError::CantCall(node.provenance.clone()));
            };

            match context.decl(ty_id).unwrap() {
                TypeDeclaration::Struct(struct_type) => {
                    if struct_type.fields.len() != fields.len() {
                        return Err(TypecheckError::WrongArgsCount(node.provenance.clone()));
                    }

                    let mut results = Ok(());
                    for (name, param_field) in struct_type.fields.iter() {
                        let Some(arg_field) = fields.get(name) else {
                            return Err(TypecheckError::MissingField(node.provenance.clone()));
                        };
                        let arg_field = context.ast.get(*arg_field);
                        match typecheck_expression(
                            arg_field,
                            outer_scopes,
                            current_scope,
                            context,
                            generator_input_ty,
                        ) {
                            Ok(arg_field_ty) => {
                                merge_results(
                                    &mut results,
                                    assert_assignable_to(
                                        context.declarations,
                                        &arg_field.provenance,
                                        param_field,
                                        arg_field_ty,
                                    ),
                                );
                            }
                            err => {
                                merge_results(&mut results, err.map(|_| {}));
                            }
                        }
                    }

                    results?;

                    ExpressionType::InstanceOf(*ty_id)
                }
                TypeDeclaration::Union(_)
                | TypeDeclaration::Interface(_)
                | TypeDeclaration::Module(_) => {
                    return Err(TypecheckError::NonStructDeclStructLiteral(
                        node.provenance.clone(),
                    ));
                }
            }
        }
        AstNodeValue::DictLiteral(entries) => {
            let mut result_key_ty = None;
            let mut result_value_ty = None;
            for (key, value) in entries.iter() {
                let key = context.ast.get(*key);
                let key_ty = typecheck_expression(
                    key,
                    outer_scopes,
                    current_scope,
                    context,
                    generator_input_ty,
                )?;
                if let Some(expected_key_ty) = result_key_ty {
                    assert_assignable_to(
                        context.declarations,
                        &key.provenance,
                        key_ty,
                        expected_key_ty,
                    )?;
                } else {
                    result_key_ty = Some(key_ty);
                }
                let value = context.ast.get(*value);
                let value_ty = typecheck_expression(
                    value,
                    outer_scopes,
                    current_scope,
                    context,
                    generator_input_ty,
                )?;
                if let Some(expected_value_ty) = result_value_ty {
                    assert_assignable_to(
                        context.declarations,
                        &value.provenance,
                        value_ty,
                        expected_value_ty,
                    )?;
                } else {
                    result_value_ty = Some(value_ty);
                }
            }

            ExpressionType::Collection(CollectionType::Dict(
                Box::new(result_key_ty.unwrap().clone()),
                Box::new(result_value_ty.unwrap().clone()),
            ))
        }
        AstNodeValue::ReferenceCountLiteral(inner) => {
            let inner_ty = typecheck_expression(
                context.ast.get(*inner),
                outer_scopes,
                current_scope,
                context,
                generator_input_ty,
            )?;
            ExpressionType::Collection(CollectionType::ReferenceCounter(Box::new(inner_ty.clone())))
        }
        AstNodeValue::CellLiteral(inner) => {
            let inner_ty = typecheck_expression(
                context.ast.get(*inner),
                outer_scopes,
                current_scope,
                context,
                generator_input_ty,
            )?;
            ExpressionType::Collection(CollectionType::Cell(Box::new(inner_ty.clone())))
        }
        AstNodeValue::TakeUnique(inner) => ExpressionType::Pointer(
            PointerKind::Unique,
            Box::new(
                typecheck_expression(
                    context.ast.get(*inner),
                    outer_scopes,
                    current_scope,
                    context,
                    generator_input_ty,
                )?
                .clone(),
            ),
        ),
        AstNodeValue::TakeRef(inner) => ExpressionType::Pointer(
            PointerKind::Shared,
            Box::new(
                typecheck_expression(
                    context.ast.get(*inner),
                    outer_scopes,
                    current_scope,
                    context,
                    generator_input_ty,
                )?
                .clone(),
            ),
        ),
        AstNodeValue::Deref(inner) => {
            let inner = context.ast.get(*inner);
            let ty = typecheck_expression(
                inner,
                outer_scopes,
                current_scope,
                context,
                generator_input_ty,
            )?;
            let (ExpressionType::Pointer(_, ty)
            | ExpressionType::Collection(CollectionType::ReferenceCounter(ty))) = ty
            else {
                return Err(TypecheckError::DereferenceNonPointer(
                    inner.provenance.clone(),
                ));
            };
            ty.as_ref().clone()
        }
        AstNodeValue::ArrayLiteral(items) => {
            if items.is_empty() {
                todo!("how to typecheck 0-length collections?");
            }
            let mut iter = items.iter();
            let ty = typecheck_expression(
                context.ast.get(*iter.next().unwrap()),
                outer_scopes,
                current_scope,
                context,
                generator_input_ty,
            )?
            .clone();
            for remaining in iter {
                let remaining = context.ast.get(*remaining);
                let this_ty = typecheck_expression(
                    remaining,
                    outer_scopes,
                    current_scope,
                    context,
                    generator_input_ty,
                )?;
                if &ty != this_ty {
                    return Err(TypecheckError::TypeMismatch {
                        provenance: remaining.provenance.clone(),
                        received: this_ty.clone(),
                        expected: ty,
                    });
                }
            }
            ExpressionType::Collection(CollectionType::Array(Box::new(ty)))
        }
        AstNodeValue::ArrayLiteralLength(value, length) => {
            let value_ty = typecheck_expression(
                context.ast.get(*value),
                outer_scopes,
                current_scope,
                context,
                generator_input_ty,
            )?;
            let length = context.ast.get(*length);
            let length_ty = typecheck_expression(
                length,
                outer_scopes,
                current_scope,
                context,
                generator_input_ty,
            )?;
            assert_assignable_to(
                context.declarations,
                &length.provenance,
                &ExpressionType::Primitive(PrimitiveType::PointerSize),
                length_ty,
            )?;
            ExpressionType::Collection(CollectionType::Array(Box::new(value_ty.clone())))
        }
        AstNodeValue::UnaryExpr(op, child) => match op {
            UnaryOp::BooleanNot => {
                let child = context.ast.get(*child);
                let child_ty = typecheck_expression(
                    child,
                    outer_scopes,
                    current_scope,
                    context,
                    generator_input_ty,
                )?;
                assert_assignable_to(
                    context.declarations,
                    &child.provenance,
                    &ExpressionType::Primitive(PrimitiveType::Bool),
                    child_ty,
                )?;
                ExpressionType::Primitive(PrimitiveType::Bool)
            }
        },
    };

    node.ty.set(ty).expect("each node should be visited once");

    Ok(node.ty.get().expect("just set"))
}

enum BindingState<'a> {
    Uninit,
    NoBinding,
    Binding(&'a String, &'a ExpressionType),
}

pub fn traverse_dots(ast: &AstArena, node: &AstNode, mut callback: impl FnMut(&str, &SourceRange)) {
    traverse_dots_recursive(ast, node, &mut callback);
}

fn traverse_dots_recursive(
    ast: &AstArena,
    node: &AstNode,
    callback: &mut impl FnMut(&str, &SourceRange),
) {
    match &node.value {
        AstNodeValue::BinExpr(BinOp::Dot, lhs, rhs) => {
            traverse_dots_recursive(ast, ast.get(*lhs), callback);
            traverse_dots_recursive(ast, ast.get(*rhs), callback);
        }
        AstNodeValue::Name { value, .. } => {
            callback(value, &node.provenance);
        }
        _ => {}
    }
}

fn typecheck_returns(
    context: &TypecheckContext,
    expected_ty: &ExpressionType,
    current: &AstNode,
) -> Result<(), TypecheckError> {
    let mut results = Ok(());
    if let AstNodeValue::Return(child) = &current.value {
        let return_ty = child
            .as_ref()
            .map(|child| context.ast.get(*child).ty.get().unwrap())
            .unwrap_or(&ExpressionType::Void);
        if !is_assignable_to(context.declarations, None, expected_ty, return_ty) {
            merge_results(
                &mut results,
                Err(TypecheckError::TypeMismatch {
                    provenance: current.provenance.clone(),
                    expected: expected_ty.clone(),
                    received: return_ty.clone(),
                }),
            );
        }
    }
    current.children(context.ast, |child| {
        merge_results(&mut results, typecheck_returns(context, expected_ty, child))
    });

    results
}

fn validate_assignment_lhs(
    context: &TypecheckContext,
    lhs: &AstNode,
) -> Result<(), TypecheckError> {
    match &lhs.value {
        // TODO: remove this if I add auto-deref later
        AstNodeValue::Name { .. } => {
            validate_assignment_lhs_ty(lhs.ty.get().unwrap(), &lhs.provenance)
        }
        AstNodeValue::Deref(inner) => {
            let inner = context.ast.get(*inner);
            let Some(ExpressionType::Pointer(kind, _)) = inner.ty.get() else {
                unreachable!()
            };
            let mut errors = if *kind == PointerKind::Shared {
                Err(TypecheckError::IllegalSharedRefMutation(
                    lhs.provenance.clone(),
                ))
            } else {
                Ok(())
            };
            merge_results(&mut errors, validate_assignment_lhs(context, inner));
            errors
        }
        AstNodeValue::BinExpr(BinOp::Dot | BinOp::Index, lhs, _) => {
            validate_assignment_lhs(context, context.ast.get(*lhs))
        }

        AstNodeValue::BinExpr(_, _, _)
        | AstNodeValue::FunctionDeclaration(_)
        | AstNodeValue::ExternFunctionBinding(_)
        | AstNodeValue::StructDeclaration(_)
        | AstNodeValue::UnionDeclaration(_)
        | AstNodeValue::InterfaceDeclaration(_)
        | AstNodeValue::RequiredFunction(_)
        | AstNodeValue::Declaration(_, _, _, _)
        | AstNodeValue::ConstDeclaration { .. }
        | AstNodeValue::Import(_)
        | AstNodeValue::Return(_)
        | AstNodeValue::Yield(_)
        | AstNodeValue::Statement(_)
        | AstNodeValue::Int(_)
        | AstNodeValue::Float(_)
        | AstNodeValue::Bool(_)
        | AstNodeValue::CharLiteral(_)
        | AstNodeValue::StringLiteral(_)
        | AstNodeValue::Null
        | AstNodeValue::If(_)
        | AstNodeValue::While(_, _)
        | AstNodeValue::Match(_)
        | AstNodeValue::Loop(_)
        | AstNodeValue::Call(_, _)
        | AstNodeValue::TakeUnique(_)
        | AstNodeValue::TakeRef(_)
        | AstNodeValue::RecordLiteral { .. }
        | AstNodeValue::DictLiteral(_)
        | AstNodeValue::ArrayLiteral(_)
        | AstNodeValue::ArrayLiteralLength(_, _)
        | AstNodeValue::Block(_)
        | AstNodeValue::UniqueType(_)
        | AstNodeValue::VoidType
        | AstNodeValue::SharedType(_)
        | AstNodeValue::ArrayType(_)
        | AstNodeValue::RcType(_)
        | AstNodeValue::DictType(_, _)
        | AstNodeValue::UnaryExpr(_, _)
        | AstNodeValue::NullableType(_)
        | AstNodeValue::GeneratorType { .. }
        | AstNodeValue::BorrowDeclaration(..)
        | AstNodeValue::ReferenceCountLiteral(_)
        | AstNodeValue::CellType(_)
        | AstNodeValue::CellLiteral(_) => {
            Err(TypecheckError::IllegalAssignmentLHS(lhs.provenance.clone()))
        }
    }
}

// Factored into its own function so it can be called at the top levle
fn typecheck_const<'a>(
    context: &TypecheckContext,
    outer_scopes: &[&HashMap<String, (AnyID, ExpressionType)>],
    current_scope: &mut HashMap<String, (AnyID, ExpressionType)>,
    node: &'a AstNode,
) -> Result<&'a ExpressionType, TypecheckError> {
    let AstNodeValue::ConstDeclaration {
        name,
        type_hint,
        value,
        variable_id,
    } = &node.value
    else {
        unreachable!()
    };
    let value = context.ast.get(*value);
    let value_ty = typecheck_expression(value, outer_scopes, current_scope, context, None)?;
    let mut result = Ok(());
    if !validate_is_const(value) {
        merge_results(
            &mut result,
            Err(TypecheckError::NonConstantInConst(value.provenance.clone())),
        );
    }
    if let Some(type_hint) = type_hint {
        let type_hint = context.ast.get(*type_hint);
        let hint_ty = resolve_type_expr(context.ast, &context.top_level_type_names, type_hint)?;
        merge_results(
            &mut result,
            assert_assignable_to(context.declarations, &value.provenance, &hint_ty, value_ty),
        );
        type_hint.ty.set(hint_ty.clone()).unwrap();
        current_scope.insert(name.clone(), ((*variable_id).into(), hint_ty));
    } else {
        current_scope.insert(name.clone(), ((*variable_id).into(), value_ty.clone()));
    }
    result?;

    node.ty
        .set(ExpressionType::Void)
        .expect("each constant should be visited once");

    Ok(node.ty.get().expect("just set"))
}

fn validate_is_const(node: &AstNode) -> bool {
    match &node.value {
        AstNodeValue::FunctionDeclaration(_)
        | AstNodeValue::ExternFunctionBinding(_)
        | AstNodeValue::StructDeclaration(_)
        | AstNodeValue::UnionDeclaration(_)
        | AstNodeValue::InterfaceDeclaration(_)
        | AstNodeValue::RequiredFunction(_)
        | AstNodeValue::Declaration(_, _, _, _)
        | AstNodeValue::BorrowDeclaration(_, _, _)
        | AstNodeValue::ConstDeclaration { .. }
        | AstNodeValue::Import(_)
        | AstNodeValue::Return(_)
        | AstNodeValue::Yield(_)
        | AstNodeValue::Null
        | AstNodeValue::Statement(_)
        | AstNodeValue::UnaryExpr(_, _)
        | AstNodeValue::BinExpr(_, _, _)
        | AstNodeValue::If(_)
        | AstNodeValue::While(_, _)
        | AstNodeValue::Loop(_)
        | AstNodeValue::Call(_, _)
        | AstNodeValue::TakeUnique(_)
        | AstNodeValue::TakeRef(_)
        | AstNodeValue::ReferenceCountLiteral(_)
        | AstNodeValue::CellLiteral(_)
        | AstNodeValue::Block(_)
        | AstNodeValue::Deref(_)
        | AstNodeValue::Match(_)
        | AstNodeValue::VoidType
        | AstNodeValue::UniqueType(_)
        | AstNodeValue::SharedType(_)
        | AstNodeValue::ArrayType(_)
        | AstNodeValue::DictType(_, _)
        | AstNodeValue::RcType(_)
        | AstNodeValue::NullableType(_)
        | AstNodeValue::CellType(_)
        | AstNodeValue::GeneratorType { .. }
        | AstNodeValue::Name { .. } => false,
        AstNodeValue::Int(_)
        | AstNodeValue::Float(_)
        | AstNodeValue::Bool(_)
        | AstNodeValue::CharLiteral(_)
        | AstNodeValue::StringLiteral(_)
        | AstNodeValue::RecordLiteral { .. }
        | AstNodeValue::DictLiteral(_)
        | AstNodeValue::ArrayLiteral(_)
        | AstNodeValue::ArrayLiteralLength(_, _) => true,
    }
}

fn ensure_no_assignment_to_reference(
    lhs: &ExpressionType,
    provenance: &SourceRange,
) -> Result<(), TypecheckError> {
    match lhs {
        ExpressionType::ReferenceToType(_)
        | ExpressionType::Null
        | ExpressionType::Unreachable
        | ExpressionType::Void
        | ExpressionType::ReferenceToFunction(_) => {
            Err(TypecheckError::IllegalAssignmentLHS(provenance.clone()))
        }
        ExpressionType::Collection(_)
        | ExpressionType::InstanceOf(_)
        | ExpressionType::Primitive(_)
        | ExpressionType::Generator { .. } => Ok(()),
        ExpressionType::Nullable(inner) => ensure_no_assignment_to_reference(inner, provenance),
        ExpressionType::Pointer(_, _) => {
            Err(TypecheckError::CantAssignToReference(provenance.clone()))
        }
        ExpressionType::TypeParameterReference(_) => todo!(),
        ExpressionType::FunctionReference { .. } => todo!(),
    }
}

fn validate_assignment_lhs_ty(
    lhs: &ExpressionType,
    provenance: &SourceRange,
) -> Result<(), TypecheckError> {
    match lhs {
        ExpressionType::ReferenceToType(_)
        | ExpressionType::Null
        | ExpressionType::Unreachable
        | ExpressionType::Void
        | ExpressionType::ReferenceToFunction(_) => {
            Err(TypecheckError::IllegalAssignmentLHS(provenance.clone()))
        }
        ExpressionType::Collection(_)
        | ExpressionType::InstanceOf(_)
        | ExpressionType::Primitive(_)
        | ExpressionType::Generator { .. }
        | ExpressionType::Pointer(PointerKind::Unique, _) => Ok(()),
        ExpressionType::Nullable(inner) => validate_assignment_lhs_ty(inner, provenance),
        ExpressionType::Pointer(PointerKind::Shared, _) => {
            Err(TypecheckError::IllegalSharedRefMutation(provenance.clone()))
        }
        ExpressionType::TypeParameterReference(_) => todo!(),
        ExpressionType::FunctionReference { .. } => todo!(),
    }
}

fn validate_lvalue(context: &TypecheckContext, lvalue: &AstNode) -> bool {
    match &lvalue.value {
        // TODO: remove this if I add auto-deref later
        AstNodeValue::Name { .. } => true,
        AstNodeValue::Deref(inner) => {
            let inner = context.ast.get(*inner);
            let Some(ExpressionType::Pointer(kind, _)) = inner.ty.get() else {
                unreachable!()
            };
            *kind == PointerKind::Unique && validate_lvalue(context, inner)
        }
        AstNodeValue::BinExpr(BinOp::Dot | BinOp::Index, lhs, _) => {
            validate_lvalue(context, context.ast.get(*lhs))
        }

        AstNodeValue::BinExpr(_, _, _)
        | AstNodeValue::FunctionDeclaration(_)
        | AstNodeValue::ExternFunctionBinding(_)
        | AstNodeValue::StructDeclaration(_)
        | AstNodeValue::UnionDeclaration(_)
        | AstNodeValue::InterfaceDeclaration(_)
        | AstNodeValue::RequiredFunction(_)
        | AstNodeValue::Declaration(_, _, _, _)
        | AstNodeValue::ConstDeclaration { .. }
        | AstNodeValue::Import(_)
        | AstNodeValue::Return(_)
        | AstNodeValue::Yield(_)
        | AstNodeValue::Statement(_)
        | AstNodeValue::Int(_)
        | AstNodeValue::Float(_)
        | AstNodeValue::Bool(_)
        | AstNodeValue::CharLiteral(_)
        | AstNodeValue::StringLiteral(_)
        | AstNodeValue::Null
        | AstNodeValue::If(_)
        | AstNodeValue::While(_, _)
        | AstNodeValue::Match(_)
        | AstNodeValue::Loop(_)
        | AstNodeValue::Call(_, _)
        | AstNodeValue::TakeUnique(_)
        | AstNodeValue::TakeRef(_)
        | AstNodeValue::RecordLiteral { .. }
        | AstNodeValue::DictLiteral(_)
        | AstNodeValue::ArrayLiteral(_)
        | AstNodeValue::ArrayLiteralLength(_, _)
        | AstNodeValue::Block(_)
        | AstNodeValue::UniqueType(_)
        | AstNodeValue::VoidType
        | AstNodeValue::SharedType(_)
        | AstNodeValue::ArrayType(_)
        | AstNodeValue::RcType(_)
        | AstNodeValue::DictType(_, _)
        | AstNodeValue::UnaryExpr(_, _)
        | AstNodeValue::NullableType(_)
        | AstNodeValue::GeneratorType { .. }
        | AstNodeValue::BorrowDeclaration(..)
        | AstNodeValue::ReferenceCountLiteral(_)
        | AstNodeValue::CellType(_)
        | AstNodeValue::CellLiteral(_) => false,
    }
}

fn resolve_name(
    name: &str,
    current_scope: &HashMap<String, (AnyID, ExpressionType)>,
    outer_scopes: &[&HashMap<String, (AnyID, ExpressionType)>],
) -> Option<(AnyID, ExpressionType)> {
    current_scope
        .get(name)
        .or(outer_scopes.iter().find_map(|scope| scope.get(name)))
        .cloned()
}

fn find_generic_bindings(
    generic_args: &mut [ExpressionType],
    left: &ExpressionType,
    right: &ExpressionType,
) {
    match (left, right) {
        // All scalar expression types cannot contain a generic binding. They may not match
        // the right side of the expression, but we're not checking that in this method
        (
            ExpressionType::Void
            | ExpressionType::Unreachable
            | ExpressionType::Primitive(_)
            | ExpressionType::InstanceOf(_)
            | ExpressionType::ReferenceToType(_)
            | ExpressionType::Null
            | ExpressionType::Collection(CollectionType::String),
            _,
        ) => {}
        (
            ExpressionType::Pointer(PointerKind::Shared, left),
            ExpressionType::Pointer(PointerKind::Shared | PointerKind::Unique, right),
        )
        | (
            ExpressionType::Pointer(PointerKind::Unique, left),
            ExpressionType::Pointer(PointerKind::Unique, right),
        )
        | (
            ExpressionType::Collection(CollectionType::Array(left)),
            ExpressionType::Collection(CollectionType::Array(right)),
        )
        | (
            ExpressionType::Collection(CollectionType::ReferenceCounter(left)),
            ExpressionType::Collection(CollectionType::ReferenceCounter(right)),
        )
        | (
            ExpressionType::Collection(CollectionType::Cell(left)),
            ExpressionType::Collection(CollectionType::Cell(right)),
        )
        | (ExpressionType::Nullable(left), ExpressionType::Nullable(right)) => {
            find_generic_bindings(generic_args, left, right);
        }
        (ExpressionType::Nullable(left), _) => find_generic_bindings(generic_args, left, right),
        (
            ExpressionType::Collection(CollectionType::Dict(left_key, left_value)),
            ExpressionType::Collection(CollectionType::Dict(right_key, right_value)),
        ) => {
            find_generic_bindings(generic_args, left_key, right_key);
            find_generic_bindings(generic_args, left_value, right_value);
        }
        (ExpressionType::TypeParameterReference(idx), _) => {
            if generic_args[*idx] == ExpressionType::Unreachable {
                generic_args[*idx] = right.clone();
            }
        }
        (
            ExpressionType::Generator {
                yield_ty: left_yield_ty,
                param_ty: left_param_ty,
            },
            ExpressionType::Generator {
                yield_ty: right_yield_ty,
                param_ty: right_param_ty,
            },
        ) => {
            find_generic_bindings(generic_args, left_yield_ty, right_yield_ty);
            find_generic_bindings(generic_args, left_param_ty, right_param_ty);
        }
        (ExpressionType::FunctionReference { .. }, _)
        | (_, ExpressionType::FunctionReference { .. }) => {
            todo!("function references only exist within the compiler")
        }
        (ExpressionType::ReferenceToFunction(_), _)
        | (_, ExpressionType::ReferenceToFunction(_)) => {
            todo!("first class functions")
        }
        (ExpressionType::Pointer(_, inner), rhs) => {
            find_generic_bindings(generic_args, inner, rhs);
        }
        (
            ExpressionType::Collection(
                CollectionType::Dict(..)
                | CollectionType::Array(_)
                | CollectionType::ReferenceCounter(_)
                | CollectionType::Cell(_),
            ),
            _,
        )
        | (ExpressionType::Generator { .. }, _) => {}
    }
}

pub fn is_assignable_to(
    context: &DeclarationContext,
    generic_args: Option<&[ExpressionType]>,
    left: &ExpressionType,
    right: &ExpressionType,
) -> bool {
    use ExpressionType::*;
    use PrimitiveType::*;

    match (left, right) {
        (TypeParameterReference(idx), _) => {
            is_assignable_to(context, generic_args, &generic_args.unwrap()[*idx], right)
        }
        (_, TypeParameterReference(_)) => {
            todo!("can you ever end up here?")
        }
        (Unreachable, Unreachable) => true,
        (Unreachable, _) => false,
        (_, Unreachable) => true,
        // Kinda a special case, but a function that returns void should accept a void block
        (Void, Void) => true,
        // Void can never be assigned to or be assigned from
        // Null can never be assigned to
        (Void, _) | (_, Void) | (Null, _) => false,

        // Handle pointers and de-referencing
        (Pointer(left_ty, left_inner), Pointer(right_ty, right_inner)) => {
            (left_ty == right_ty
                || *left_ty == PointerKind::Shared && *right_ty == PointerKind::Unique)
                && is_assignable_to(context, generic_args, left_inner, right_inner)
        }
        // TODO: auto-dereference in the IR
        (left, Pointer(_, right_inner)) => {
            is_assignable_to(context, generic_args, left, right_inner)
        }
        // TODO: support auto-dereferencing on lhs
        (Pointer(_, _), _right) => false,

        // Nullability
        (Nullable(_), Null) => true,
        (_, Null) => false,
        (Nullable(left), Nullable(right)) => is_assignable_to(context, generic_args, left, right),
        (Nullable(left), right) => is_assignable_to(context, generic_args, left, right),
        (_, Nullable(_)) => false,

        (Primitive(Float32), Primitive(Int32))
        | (Primitive(Float64), Primitive(Int32))
        | (Primitive(Float64), Primitive(Int64))
        | (Primitive(Int64), Primitive(Int32))
        | (Primitive(Float64), Primitive(Float32))
        | (Primitive(PointerSize), Primitive(Int32)) => true,
        (Primitive(left), Primitive(right)) => left == right,
        (Primitive(_), _) => false,

        (InstanceOf(left), InstanceOf(right)) => {
            let left = &context.id_to_decl[left];
            let right = &context.id_to_decl[right];
            use TypeDeclaration::*;
            match (left, right) {
                (Struct(_), Struct(_)) => left == right,
                (Struct(_), _) => false,

                (Union(_), Union(_)) => left == right,
                (Union(_), _) => false,

                (
                    Interface(InterfaceType {
                        associated_functions: lhs_assoc,
                        ..
                    }),
                    Struct(StructType {
                        associated_functions: rhs_assoc,
                        ..
                    }),
                ) => lhs_assoc.iter().all(|(name, lhs_ty)| {
                    let Some(rhs_ty) = rhs_assoc.get(name) else {
                        return false;
                    };
                    let lhs = &context.id_to_func[lhs_ty];
                    let rhs = &context.id_to_func[rhs_ty];
                    // Ignore the first argument to both associated functions -
                    // the type will differ because it's a self param
                    lhs.params[1..]
                        .iter()
                        .zip(rhs.params[1..].iter())
                        .all(|(lhs, rhs)| lhs == rhs)
                        && lhs.returns == rhs.returns
                }),
                (Interface(_), Interface(_)) => left == right,
                (Interface(_), Union(_)) => todo!(),

                (_, Module(_)) => false,
                // You can never assign to a module
                (Module(_), _) => false,
            }
        }
        (
            Generator {
                yield_ty: left_yield_ty,
                param_ty: left_param_ty,
            },
            Generator {
                yield_ty: right_yield_ty,
                param_ty: right_param_ty,
            },
        ) => left_yield_ty == right_yield_ty && left_param_ty == right_param_ty,
        (Generator { .. }, _) | (_, Generator { .. }) => false,

        (InstanceOf(_), Primitive(_))
        | (InstanceOf(_), Collection(_))
        | (Collection(_), Primitive(_))
        | (Collection(_), InstanceOf(_)) => false,

        (Collection(CollectionType::Array(left)), Collection(CollectionType::Array(right)))
        | (
            Collection(CollectionType::ReferenceCounter(left)),
            Collection(CollectionType::ReferenceCounter(right)),
        )
        | (Collection(CollectionType::Cell(left)), Collection(CollectionType::Cell(right))) => {
            left == right
        }
        (
            Collection(CollectionType::Dict(left_key, left_value)),
            Collection(CollectionType::Dict(right_key, right_value)),
        ) => left_key == right_key && left_value == right_value,
        (Collection(CollectionType::String), Collection(CollectionType::String)) => true,
        (Collection(_), Collection(_)) => false,

        (ExpressionType::FunctionReference { .. }, _)
        | (_, ExpressionType::FunctionReference { .. }) => {
            todo!("function references only exist within the compiler")
        }
        (ExpressionType::ReferenceToFunction(_), _)
        | (_, ExpressionType::ReferenceToFunction(_)) => {
            todo!("first class functions")
        }

        (ReferenceToType(_), _) | (_, ReferenceToType(_)) => todo!("{:?} = {:?}", left, right),
    }
}

pub fn fully_dereference(ty: &ExpressionType) -> &ExpressionType {
    if let ExpressionType::Pointer(_, inner) = ty {
        fully_dereference(inner)
    } else {
        ty
    }
}

pub fn shallow_dereference(ty: &ExpressionType) -> &ExpressionType {
    if let ExpressionType::Pointer(_, inner) = ty {
        inner
    } else {
        ty
    }
}

fn assert_assignable_to(
    declarations: &DeclarationContext,
    provenance: &SourceRange,
    lhs: &ExpressionType,
    rhs: &ExpressionType,
) -> Result<(), TypecheckError> {
    if is_assignable_to(declarations, None, lhs, rhs) {
        Ok(())
    } else {
        Err(TypecheckError::TypeMismatch {
            provenance: provenance.clone(),
            received: lhs.clone(),
            expected: rhs.clone(),
        })
    }
}
