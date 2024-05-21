use std::collections::HashMap;

use crate::{
    id::TypeID,
    multi_error::{merge_result_list, merge_results},
    parser::{
        AstNode, AstNodeValue, FunctionDeclarationValue, FunctionHeaderValue,
        InterfaceDeclarationValue, NameAndType, StructDeclarationValue, UnionDeclarationValue,
        UnionDeclarationVariant,
    },
};

use super::{
    CollectionType, ExpressionType, FuncType, InterfaceType, PointerKind, PrimitiveType,
    StaticDeclaration, StructType, TypecheckError, UnionType,
};

pub fn resolve_module(
    source: &[AstNode<'_>],
) -> Result<HashMap<String, StaticDeclaration>, TypecheckError> {
    let mut names_to_declarations = HashMap::new();
    for statement in source.iter() {
        match &statement.value {
            AstNodeValue::StructDeclaration(StructDeclarationValue { name, .. })
            | AstNodeValue::UnionDeclaration(UnionDeclarationValue { name, .. })
            | AstNodeValue::InterfaceDeclaration(InterfaceDeclarationValue { name, .. })
            | AstNodeValue::FunctionDeclaration(FunctionDeclarationValue { name, .. })
            | AstNodeValue::ExternFunctionBinding(FunctionHeaderValue { name, .. }) => {
                names_to_declarations.insert(name.clone(), statement);
            }
            _ => {}
        }
    }

    let mut top_level_decls = resolve_top_level_declarations(&names_to_declarations)?;

    let mut id_decls = HashMap::new();
    for decl in top_level_decls.values() {
        decl.visit(&mut |decl: &StaticDeclaration| {
            id_decls.insert(decl.id(), decl);
        });
    }

    let mut affinity = HashMap::new();
    for decl in top_level_decls.values() {
        is_decl_affine(&id_decls, decl, &mut affinity);
    }
    for decl in top_level_decls.values_mut() {
        match decl {
            StaticDeclaration::Struct(decl) => decl.is_affine = affinity[&decl.id],
            StaticDeclaration::Union(decl) => decl.is_affine = affinity[&decl.id],
            StaticDeclaration::Func(_)
            | StaticDeclaration::Interface(_)
            | StaticDeclaration::Module(_) => {}
        }
    }

    Ok(top_level_decls)
}

fn is_decl_affine(
    id_to_decl: &HashMap<TypeID, &StaticDeclaration>,
    decl: &StaticDeclaration,
    affine_types: &mut HashMap<TypeID, bool>,
) -> bool {
    let id = decl.id();
    if let Some(affinity) = affine_types.get(&id) {
        return *affinity;
    }
    if decl.is_affine() {
        affine_types.insert(id, true);
        return true;
    }
    let is_children_affine = match decl {
        StaticDeclaration::Struct(decl) => decl
            .fields
            .values()
            .filter_map(|expr| expr.id())
            .any(|id| is_decl_affine(id_to_decl, id_to_decl[id], affine_types)),
        StaticDeclaration::Union(decl) => decl
            .variants
            .values()
            .filter_map(|expr| expr.as_ref()?.id())
            .any(|id| is_decl_affine(id_to_decl, id_to_decl[id], affine_types)),
        StaticDeclaration::Func(_)
        | StaticDeclaration::Interface(_)
        | StaticDeclaration::Module(_) => false,
    };

    affine_types.insert(id, is_children_affine);
    is_children_affine
}

pub fn resolve_top_level_declarations(
    names_to_declarations: &HashMap<String, &AstNode<'_>>,
) -> Result<HashMap<String, StaticDeclaration>, TypecheckError> {
    let name_to_type_id = names_to_declarations
        .iter()
        .map(|(name, _)| (name.as_str(), TypeID::new()))
        .collect();
    let mut declarations = HashMap::new();
    let mut decl_result = Ok(());
    for (name, node) in names_to_declarations.iter() {
        match resolve_declaration(&name_to_type_id, node, false) {
            Ok(declaration) => {
                declarations.insert(name.clone(), declaration);
            }
            Err(err) => merge_results(&mut decl_result, Err(err)),
        }
    }
    decl_result?;
    Ok(declarations)
}

fn resolve_declaration(
    names_to_type_id: &HashMap<&str, TypeID>,
    node: &AstNode<'_>,
    is_associated: bool,
) -> Result<StaticDeclaration, TypecheckError> {
    Ok(match &node.value {
        AstNodeValue::StructDeclaration(StructDeclarationValue {
            fields,
            associated_functions,
            name,
            properties,
        }) => {
            let fields: HashMap<_, _> = merge_result_list(fields.iter().map(
                |NameAndType {
                     name,
                     ty,
                     provenance,
                 }| {
                    let ty = resolve_type_expr(names_to_type_id, ty)?;
                    if matches!(ty, ExpressionType::Pointer(_, _)) {
                        return Err(TypecheckError::IllegalReferenceInsideDataType(
                            provenance.clone(),
                        ));
                    }
                    Ok((name.clone(), ty))
                },
            ))?;
            let associated_functions = associated_functions
                .iter()
                .map(|node| {
                    Ok(match &node.value {
                        AstNodeValue::FunctionDeclaration(FunctionDeclarationValue {
                            name,
                            ..
                        }) => (
                            name.clone(),
                            resolve_declaration(names_to_type_id, node, true)?,
                        ),
                        _ => panic!(
                            "Associated function should not be anything but function declaration"
                        ),
                    })
                })
                .collect::<Result<HashMap<_, _>, _>>()?;
            let mut is_affine = false;
            for property in properties.iter() {
                match property.as_str() {
                    "Move" => is_affine = true,
                    _ => {
                        return Err(TypecheckError::UnknownProperty(
                            property.clone(),
                            node.provenance.clone(),
                        ))
                    }
                }
            }

            StaticDeclaration::Struct(StructType {
                id: names_to_type_id[name.as_str()],
                fields,
                associated_functions,
                is_affine,
            })
        }
        AstNodeValue::InterfaceDeclaration(InterfaceDeclarationValue {
            associated_functions,
            name,
            ..
        }) => {
            let associated_functions = associated_functions
                .iter()
                .map(|node| {
                    Ok(match &node.value {
                        AstNodeValue::RequiredFunction(FunctionHeaderValue { name, .. }) => (
                            name.clone(),
                            resolve_declaration(names_to_type_id, node, true)?,
                        ),
                        AstNodeValue::FunctionDeclaration(FunctionDeclarationValue {
                            name,
                            ..
                        }) => (
                            name.clone(),
                            resolve_declaration(names_to_type_id, node, true)?,
                        ),
                        _ => panic!(
                            "Associated function should not be anything but function declaration"
                        ),
                    })
                })
                .collect::<Result<HashMap<_, _>, _>>()?;

            StaticDeclaration::Interface(InterfaceType {
                id: names_to_type_id[name.as_str()],
                associated_functions,
            })
        }
        AstNodeValue::UnionDeclaration(UnionDeclarationValue {
            variants: variant_ast,
            name,
            properties,
        }) => {
            let variants = merge_result_list(variant_ast.iter().map(|variant| {
                Ok(match variant {
                    UnionDeclarationVariant::WithValue(NameAndType {
                        name,
                        ty,
                        provenance,
                    }) => {
                        let ty = resolve_type_expr(names_to_type_id, ty)?;
                        if matches!(ty, ExpressionType::Pointer(_, _)) {
                            return Err(TypecheckError::IllegalReferenceInsideDataType(
                                provenance.clone(),
                            ));
                        }
                        (name.clone(), Some(ty))
                    }
                    UnionDeclarationVariant::WithoutValue(name) => (name.clone(), None),
                })
            }))?;

            let mut is_affine = false;
            for property in properties.iter() {
                match property.as_str() {
                    "Move" => is_affine = true,
                    _ => {
                        return Err(TypecheckError::UnknownProperty(
                            property.clone(),
                            node.provenance.clone(),
                        ))
                    }
                }
            }

            StaticDeclaration::Union(UnionType {
                id: names_to_type_id[name.as_str()],
                variant_order: variant_ast
                    .iter()
                    .map(|variant| match variant {
                        UnionDeclarationVariant::WithValue(name_and_type) => {
                            name_and_type.name.clone()
                        }
                        UnionDeclarationVariant::WithoutValue(name) => name.clone(),
                    })
                    .collect(),
                variants,
                is_affine,
            })
        }
        AstNodeValue::FunctionDeclaration(FunctionDeclarationValue {
            params,
            returns,
            id,
            name,
            is_coroutine,
            ..
        }) => StaticDeclaration::Func(FuncType {
            id: names_to_type_id
                .get(name.as_str())
                .copied()
                .unwrap_or_else(TypeID::new),
            func_id: *id,
            type_param_count: 0,
            params: params
                .iter()
                .map(|(_, NameAndType { ty: type_, .. })| {
                    resolve_type_expr(names_to_type_id, type_)
                })
                .collect::<Result<Vec<_>, _>>()?,
            returns: returns
                .as_ref()
                .map(|returns| resolve_type_expr(names_to_type_id, returns))
                .unwrap_or(Ok(ExpressionType::Void))?,
            is_associated,
            is_coroutine: *is_coroutine,
            provenance: Some(node.provenance.clone()),
        }),
        AstNodeValue::RequiredFunction(FunctionHeaderValue {
            params,
            returns,
            id,
            name,
        })
        | AstNodeValue::ExternFunctionBinding(FunctionHeaderValue {
            params,
            returns,
            id,
            name,
        }) => StaticDeclaration::Func(FuncType {
            id: names_to_type_id
                .get(name.as_str())
                .copied()
                .unwrap_or_else(TypeID::new),
            func_id: *id,
            type_param_count: 0,
            params: params
                .iter()
                .map(|NameAndType { ty: type_, .. }| resolve_type_expr(names_to_type_id, type_))
                .collect::<Result<Vec<_>, _>>()?,
            returns: returns
                .as_ref()
                .map(|returns| resolve_type_expr(names_to_type_id, returns))
                .unwrap_or(Ok(ExpressionType::Void))?,
            is_associated,
            is_coroutine: false,
            provenance: Some(node.provenance.clone()),
        }),
        _ => panic!("internal compiler error: unexpected decl node"),
    })
}

pub fn resolve_type_expr(
    name_to_type_id: &HashMap<&str, TypeID>,
    node: &AstNode<'_>,
) -> Result<ExpressionType, TypecheckError> {
    Ok(match &node.value {
        AstNodeValue::Name { value: name, .. } => match name.as_str() {
            "bool" => ExpressionType::Primitive(PrimitiveType::Bool),
            "i32" => ExpressionType::Primitive(PrimitiveType::Int32),
            "f32" => ExpressionType::Primitive(PrimitiveType::Float32),
            "i64" => ExpressionType::Primitive(PrimitiveType::Int64),
            "f64" => ExpressionType::Primitive(PrimitiveType::Float64),
            "char" => ExpressionType::Primitive(PrimitiveType::Char),
            "string" => ExpressionType::Collection(CollectionType::String),
            other => ExpressionType::InstanceOf(
                *name_to_type_id
                    .get(other)
                    .ok_or(TypecheckError::NameNotFound(node.provenance.clone()))?,
            ),
        },
        AstNodeValue::VoidType => ExpressionType::Void,
        AstNodeValue::UniqueType(inner) => ExpressionType::Pointer(
            PointerKind::Unique,
            Box::new(resolve_type_expr(name_to_type_id, inner)?),
        ),
        AstNodeValue::SharedType(inner) => ExpressionType::Pointer(
            PointerKind::Shared,
            Box::new(resolve_type_expr(name_to_type_id, inner)?),
        ),
        AstNodeValue::ArrayType(inner) => ExpressionType::Collection(CollectionType::Array(
            Box::new(resolve_type_expr(name_to_type_id, inner)?),
        )),
        AstNodeValue::DictType(key, value) => ExpressionType::Collection(CollectionType::Dict(
            Box::new(resolve_type_expr(name_to_type_id, key)?),
            Box::new(resolve_type_expr(name_to_type_id, value)?),
        )),
        AstNodeValue::NullableType(inner) => {
            ExpressionType::Nullable(Box::new(resolve_type_expr(name_to_type_id, inner)?))
        }
        AstNodeValue::GeneratorType { yield_ty, param_ty } => ExpressionType::Generator {
            yield_ty: Box::new(resolve_type_expr(name_to_type_id, yield_ty)?),
            param_ty: Box::new(resolve_type_expr(name_to_type_id, param_ty)?),
        },
        AstNodeValue::FunctionDeclaration(_)
        | AstNodeValue::RequiredFunction(_)
        | AstNodeValue::ExternFunctionBinding(_)
        | AstNodeValue::StructDeclaration(_)
        | AstNodeValue::UnionDeclaration(_)
        | AstNodeValue::InterfaceDeclaration(_)
        | AstNodeValue::Declaration(_, _, _, _)
        | AstNodeValue::Import(_)
        | AstNodeValue::Return(_)
        | AstNodeValue::Yield(_)
        | AstNodeValue::Statement(_)
        | AstNodeValue::Deref(_)
        | AstNodeValue::Int(_)
        | AstNodeValue::Null
        | AstNodeValue::Float(_)
        | AstNodeValue::Bool(_)
        | AstNodeValue::BinExpr(_, _, _)
        | AstNodeValue::If(_)
        | AstNodeValue::While(_, _)
        | AstNodeValue::Loop(_)
        | AstNodeValue::Call(_, _)
        | AstNodeValue::TakeUnique(_)
        | AstNodeValue::TakeRef(_)
        | AstNodeValue::RecordLiteral { .. }
        | AstNodeValue::ArrayLiteral(_)
        | AstNodeValue::ArrayLiteralLength(_, _)
        | AstNodeValue::Block(_)
        | AstNodeValue::StringLiteral(_)
        | AstNodeValue::CharLiteral(_)
        | AstNodeValue::UnaryExpr(_, _)
        | AstNodeValue::DictLiteral(_)
        | AstNodeValue::Match(_)
        | AstNodeValue::BorrowDeclaration(..) => {
            // TODO: report error
            panic!("Illegal in type name");
        }
    })
}
