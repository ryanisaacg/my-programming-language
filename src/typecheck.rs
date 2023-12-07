use std::collections::HashMap;

use thiserror::Error;

use crate::{
    id::ID,
    parser::{
        AstNode, AstNodeValue, BinOp, FunctionDeclarationValue, IfDeclaration,
        InterfaceDeclarationValue, StructDeclarationValue,
    },
    provenance::SourceRange,
};

use self::control_flow_graph::build_control_flow_graph;

mod control_flow_graph;
pub mod resolve;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExpressionType {
    Primitive(PrimitiveType),
    DeclaredType(ID),
    Pointer(PointerKind, Box<ExpressionType>),
    Array(Box<ExpressionType>),
    Null,
    Nullable(Box<ExpressionType>),
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PointerKind {
    Shared,
    Unique,
}

#[derive(Debug, PartialEq, Eq)]
pub enum StaticDeclaration {
    Func(FuncType),
    Struct(StructType),
    Interface(InterfaceType),
    Union(UnionType),
    Module(ModuleType),
}

impl StaticDeclaration {
    pub fn id(&self) -> ID {
        match self {
            StaticDeclaration::Func(inner) => inner.id,
            StaticDeclaration::Struct(inner) => inner.id,
            StaticDeclaration::Interface(inner) => inner.id,
            StaticDeclaration::Union(inner) => inner.id,
            StaticDeclaration::Module(inner) => inner.id,
        }
    }

    pub fn expr(&self) -> ExpressionType {
        ExpressionType::DeclaredType(self.id())
    }

    pub fn visit<'a>(&'a self, visitor: &mut impl FnMut(&'a StaticDeclaration)) {
        visitor(self);
        match self {
            StaticDeclaration::Module(module) => {
                for child in module.exports.values() {
                    child.visit(visitor);
                }
            }
            StaticDeclaration::Struct(StructType {
                associated_functions,
                ..
            })
            | StaticDeclaration::Interface(InterfaceType {
                associated_functions,
                ..
            }) => {
                for child in associated_functions.values() {
                    child.visit(visitor);
                }
            }
            StaticDeclaration::Func(_) | StaticDeclaration::Union(_) => {}
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct ModuleType {
    pub id: ID,
    pub exports: HashMap<String, StaticDeclaration>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct StructType {
    pub id: ID,
    pub fields: HashMap<String, ExpressionType>,
    pub associated_functions: HashMap<String, StaticDeclaration>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct FuncType {
    pub id: ID,
    pub params: Vec<ExpressionType>,
    pub returns: ExpressionType,
}

#[derive(Debug, PartialEq, Eq)]
pub struct UnionType {
    pub id: ID,
    pub variants: HashMap<String, ExpressionType>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct InterfaceType {
    pub id: ID,
    pub fields: HashMap<String, ExpressionType>,
    pub associated_functions: HashMap<String, StaticDeclaration>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PrimitiveType {
    Char,
    String,
    Int32,
    Float32,
    Int64,
    Float64,
    Bool,
    // TODO: take out of PrimitiveType
    Void,
}

// TODO: get the lifetimes out of these typecheck errors
#[derive(Debug, Error)]
pub enum TypecheckError {
    #[error("arithmetic")]
    ArithmeticMismatch(SourceRange), // TODO
    #[error("mismatched types: received {received:?}, expected {expected:?}")]
    TypeMismatch {
        // TODO: provenance
        expected: ExpressionType,
        received: ExpressionType,
    },
    #[error("declaration for {0:?} not found")]
    NameNotFound(SourceRange), // TODO
    #[error("can't call")]
    CantCall(SourceRange), // TODO
    #[error("wrong args count")]
    WrongArgsCount(SourceRange),
    #[error("missing field")]
    MissingField(SourceRange),
}

struct Declarations<'a> {
    name_to_expr: HashMap<String, (ID, ExpressionType)>,
    id_to_decl: HashMap<ID, &'a StaticDeclaration>,
}

impl<'a> Declarations<'a> {
    fn decl(&self, id: &ID) -> Option<&StaticDeclaration> {
        self.id_to_decl.get(id).copied()
    }

    fn id_to_func(&self, id: &ID) -> &FuncType {
        let expr = self.decl(id).expect("function with ID should exist");
        match expr {
            StaticDeclaration::Func(inner) => inner,
            _ => panic!("ID unexpectedly pointed to non-function"),
        }
    }

    fn id_to_struct(&self, id: &ID) -> &StructType {
        let expr = self.decl(id).expect("struct with ID should exist");
        match expr {
            StaticDeclaration::Struct(inner) => inner,
            _ => panic!("ID unexpectedly pointed to non-struct"),
        }
    }
}

pub struct TypecheckedFile<'a> {
    pub functions: Vec<TypecheckedFunction<'a>>,
    pub associated_functions: HashMap<ID, Vec<TypecheckedFunction<'a>>>,
    pub top_level_statements: Vec<&'a AstNode<'a>>,
}

pub struct TypecheckedFunction<'a> {
    pub id: ID,
    pub name: String,
    pub func: &'a FunctionDeclarationValue<'a>,
}

// TODO: pass import namespace in
pub fn typecheck<'a>(
    file: impl Iterator<Item = &'a AstNode<'a>>,
    current_module_name: &str,
    declarations: &HashMap<String, StaticDeclaration>,
) -> Result<TypecheckedFile<'a>, TypecheckError> {
    // TODO: verify validity of type and function declarations

    let mut name_to_expr = HashMap::new();
    let mut id_to_decl = HashMap::new();

    for (name, value) in declarations {
        match value {
            StaticDeclaration::Module(module) if current_module_name == name => {
                for (name, value) in module.exports.iter() {
                    name_to_expr.insert(name.clone(), (value.id(), value.expr()));
                }
            }
            _ => {
                name_to_expr.insert(name.clone(), (value.id(), value.expr()));
            }
        }
        value.visit(&mut |decl| {
            id_to_decl.insert(decl.id(), decl);
        });
    }

    let context = Declarations {
        name_to_expr,
        id_to_decl,
    };

    // TODO: handle free-floating statements

    let mut functions = Vec::new();
    let mut top_level_statements = Vec::new();
    let mut top_level_scope = HashMap::new();
    let mut associated_functions = HashMap::new();

    for statement in file {
        typecheck_node(
            statement,
            &context,
            &mut top_level_scope,
            &mut functions,
            &mut top_level_statements,
            &mut associated_functions,
        )?;
    }

    Ok(TypecheckedFile {
        functions,
        associated_functions,
        top_level_statements,
    })
}

fn typecheck_node<'ast>(
    statement: &'ast AstNode<'ast>,
    context: &Declarations,
    top_level_scope: &mut HashMap<String, (ID, ExpressionType)>,
    functions: &mut Vec<TypecheckedFunction<'ast>>,
    top_level_statements: &mut Vec<&AstNode<'ast>>,
    associated_functions_for_type: &mut HashMap<ID, Vec<TypecheckedFunction<'ast>>>,
) -> Result<(), TypecheckError> {
    match &statement.value {
        AstNodeValue::FunctionDeclaration(func) => {
            // TODO: bubble errors
            typecheck_function(&statement.id, func, context).unwrap();
            functions.push(TypecheckedFunction {
                id: statement.id,
                name: func.name.clone(),
                func,
            });
        }
        AstNodeValue::StructDeclaration(StructDeclarationValue {
            associated_functions,
            ..
        })
        | AstNodeValue::InterfaceDeclaration(InterfaceDeclarationValue {
            associated_functions,
            ..
        }) => {
            let associated_functions = associated_functions
                .iter()
                .filter_map(|function| {
                    let AstNodeValue::FunctionDeclaration(func) = &function.value else {
                        return None;
                    };
                    typecheck_function(&function.id, func, context).unwrap();
                    Some(TypecheckedFunction {
                        id: statement.id,
                        name: func.name.clone(),
                        func,
                    })
                })
                .collect();
            associated_functions_for_type.insert(statement.id, associated_functions);
        }
        // These nodes don't execute anything and therefore don't need to be typechecked
        AstNodeValue::Import(_)
        | AstNodeValue::UnionDeclaration(_)
        | AstNodeValue::ExternFunctionBinding(_) => {}
        _ => {
            typecheck_expression(
                statement,
                &[&context.name_to_expr],
                top_level_scope,
                &context,
            )?;
            top_level_statements.push(statement);
        }
    }

    Ok(())
}

// TODO: some sort of data structure to store the results?
fn typecheck_function<'a>(
    id: &ID,
    function: &'a FunctionDeclarationValue<'a>,
    context: &Declarations,
) -> Result<(), TypecheckError> {
    let function_type = context.id_to_func(id);
    let parameters = function
        .params
        .iter()
        .zip(function_type.params.iter())
        .map(|(name, param)| (name.name.clone(), (name.id, param.clone())))
        .collect();

    let return_value = typecheck_expression(
        &function.body,
        &[&context.name_to_expr, &parameters],
        &mut HashMap::new(),
        context,
    )?;
    let _cfg = build_control_flow_graph(&function.body);
    // TODO: check all the return values for matching
    // TODO: check to see reachability

    if !is_assignable_to(&function_type.returns, &return_value) {
        return Err(TypecheckError::TypeMismatch {
            expected: function_type.returns.clone(),
            received: return_value.clone(),
        });
    }

    Ok(())
}

// TODO: allow ; to turn expressions into void
// TODO: if-else need to unify
fn typecheck_expression<'a>(
    node: &'a AstNode<'a>,
    outer_scopes: &[&HashMap<String, (ID, ExpressionType)>],
    current_scope: &mut HashMap<String, (ID, ExpressionType)>,
    context: &Declarations,
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
        | AstNodeValue::SharedType(_)
        | AstNodeValue::ArrayType(_)
        | AstNodeValue::NullableType(_) => {
            panic!("illegal type expression in function body");
        }
        AstNodeValue::Statement(inner) => {
            typecheck_expression(inner, outer_scopes, current_scope, context)?;
            ExpressionType::Primitive(PrimitiveType::Void)
        }
        AstNodeValue::Declaration(name, value) => {
            // TODO: do I want shadowing? currently this shadows
            let value = typecheck_expression(value, outer_scopes, current_scope, context)?;
            current_scope.insert(name.clone(), (node.id, value.clone()));

            ExpressionType::Primitive(PrimitiveType::Void)
        }
        // TODO: You can only from within a function
        AstNodeValue::Return(returned) => {
            typecheck_expression(returned, outer_scopes, current_scope, context)?;

            ExpressionType::Primitive(PrimitiveType::Void)
        }
        AstNodeValue::Name {
            value: name,
            referenced_id,
        } => {
            let (ref_id, expr) = resolve_name(name, current_scope, outer_scopes)
                .ok_or(TypecheckError::NameNotFound(node.provenance.clone()))?;
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
            if *constant as f32 as f64 == *constant {
                ExpressionType::Primitive(PrimitiveType::Float32)
            } else {
                ExpressionType::Primitive(PrimitiveType::Float64)
            }
        }
        AstNodeValue::StringLiteral(_) => ExpressionType::Primitive(PrimitiveType::String),
        AstNodeValue::CharLiteral(_) => ExpressionType::Primitive(PrimitiveType::Char),
        AstNodeValue::Null => ExpressionType::Null,
        AstNodeValue::Bool(_) => ExpressionType::Primitive(PrimitiveType::Bool),
        AstNodeValue::BinExpr(BinOp::Dot, left, right) => {
            let left = typecheck_expression(left, outer_scopes, current_scope, context)?;
            let ExpressionType::DeclaredType(id) = left else {
                panic!("TODO: left side of dot operator");
            };
            let AstNodeValue::Name { value: name, .. } = &right.value else {
                panic!("TODO: right side of dot operator");
            };
            // TODO: fallible
            let lhs_type = context.decl(&id);
            match lhs_type {
                Some(
                    StaticDeclaration::Struct(StructType { fields, .. })
                    | StaticDeclaration::Interface(InterfaceType { fields, .. }),
                ) => fields.get(name).expect("TODO: field is present").clone(),
                Some(StaticDeclaration::Union(lhs_type)) => ExpressionType::Nullable(Box::new(
                    lhs_type
                        .variants
                        .get(name)
                        .expect("TODO: variant is present")
                        .clone(),
                )),
                Some(StaticDeclaration::Module(lhs_type)) => lhs_type
                    .exports
                    .get(name)
                    .expect("TODO: export is present")
                    .expr(),
                _ => todo!("illegal lhs type"),
            }
        }
        AstNodeValue::BinExpr(BinOp::Index, _, _) => todo!(),
        AstNodeValue::BinExpr(
            BinOp::Add | BinOp::Subtract | BinOp::Multiply | BinOp::Divide,
            left,
            right,
        ) => {
            let left = typecheck_expression(left, outer_scopes, current_scope, context)?;
            let ExpressionType::Primitive(left) = fully_dereference(&left) else {
                return Err(TypecheckError::ArithmeticMismatch(node.provenance.clone()));
            };
            let right = typecheck_expression(right, outer_scopes, current_scope, context)?;
            let ExpressionType::Primitive(right) = fully_dereference(&right) else {
                return Err(TypecheckError::ArithmeticMismatch(node.provenance.clone()));
            };
            if left == right {
                ExpressionType::Primitive(*left)
            } else {
                return Err(TypecheckError::ArithmeticMismatch(node.provenance.clone()));
            }
        }
        // TODO: non-numeric equality
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
            let left = typecheck_expression(left, outer_scopes, current_scope, context)?;
            let ExpressionType::Primitive(_) = fully_dereference(&left) else {
                return Err(TypecheckError::ArithmeticMismatch(node.provenance.clone()));
            };
            let right = typecheck_expression(right, outer_scopes, current_scope, context)?;
            let ExpressionType::Primitive(_) = fully_dereference(&right) else {
                return Err(TypecheckError::ArithmeticMismatch(node.provenance.clone()));
            };

            ExpressionType::Primitive(PrimitiveType::Bool)
        }
        AstNodeValue::BinExpr(BinOp::Assignment, left, right) => {
            // TODO: ensure left is a valid lvalue
            let left = typecheck_expression(left, outer_scopes, current_scope, context)?;
            let right = typecheck_expression(right, outer_scopes, current_scope, context)?;

            if !is_assignable_to(&left, &right) {
                return Err(TypecheckError::TypeMismatch {
                    received: right.clone(),
                    expected: left.clone(),
                });
            }

            ExpressionType::Primitive(PrimitiveType::Void)
        }
        AstNodeValue::BinExpr(
            BinOp::AddAssign | BinOp::SubtractAssign | BinOp::MultiplyAssign | BinOp::DivideAssign,
            left,
            right,
        ) => {
            // TODO: ensure left is a valid lvalue
            let left = typecheck_expression(left, outer_scopes, current_scope, context)?;
            let right = typecheck_expression(right, outer_scopes, current_scope, context)?;

            if !matches!(fully_dereference(&left), ExpressionType::Primitive(_))
                || !matches!(fully_dereference(&right), ExpressionType::Primitive(_))
            {
                return Err(TypecheckError::ArithmeticMismatch(node.provenance.clone()));
            }

            if !is_assignable_to(&left, &right) {
                return Err(TypecheckError::TypeMismatch {
                    received: right.clone(),
                    expected: left.clone(),
                });
            }

            ExpressionType::Primitive(PrimitiveType::Void)
        }
        AstNodeValue::While(condition, body) => {
            let condition = typecheck_expression(condition, outer_scopes, current_scope, context)?;
            let condition_deref = fully_dereference(&condition);
            if !matches!(
                condition_deref,
                ExpressionType::Primitive(PrimitiveType::Bool)
            ) {
                return Err(TypecheckError::TypeMismatch {
                    received: condition.clone(),
                    expected: ExpressionType::Primitive(PrimitiveType::Bool),
                });
            }

            typecheck_expression(body, outer_scopes, current_scope, context)?;

            ExpressionType::Primitive(PrimitiveType::Void)
        }
        AstNodeValue::If(IfDeclaration {
            condition,
            if_branch,
            else_branch,
        }) => {
            let condition = typecheck_expression(condition, outer_scopes, current_scope, context)?;
            let condition_deref = fully_dereference(&condition);
            if !matches!(
                condition_deref,
                ExpressionType::Primitive(PrimitiveType::Bool)
            ) {
                return Err(TypecheckError::TypeMismatch {
                    received: condition.clone(),
                    expected: ExpressionType::Primitive(PrimitiveType::Bool),
                });
            }

            let if_branch = typecheck_expression(if_branch, outer_scopes, current_scope, context)?;
            let else_branch = else_branch
                .as_ref()
                .map(|else_branch| {
                    typecheck_expression(else_branch, outer_scopes, current_scope, context)
                })
                .transpose()?;

            match else_branch {
                Some(else_branch) => {
                    if if_branch == else_branch {
                        if_branch.clone()
                    } else {
                        // TODO: typecheck error here UNLESS there's a break/return
                        panic!("if and else don't match");
                    }
                }
                None => ExpressionType::Primitive(PrimitiveType::Void),
            }
        }
        AstNodeValue::Block(children) => {
            let mut scopes: Vec<&HashMap<_, _>> = Vec::with_capacity(outer_scopes.len() + 1);
            scopes.push(current_scope);
            scopes.extend_from_slice(outer_scopes);

            let mut child_scope = HashMap::new();
            let mut expr_ty = ExpressionType::Primitive(PrimitiveType::Void);
            for (index, child) in children.iter().enumerate() {
                expr_ty =
                    typecheck_expression(child, &scopes[..], &mut child_scope, context)?.clone();
                if index != children.len() - 1
                    && expr_ty != ExpressionType::Primitive(PrimitiveType::Void)
                {
                    return Err(TypecheckError::TypeMismatch {
                        expected: ExpressionType::Primitive(PrimitiveType::Void),
                        received: expr_ty,
                    });
                }
            }
            expr_ty
        }
        AstNodeValue::Call(func, args) => {
            let ExpressionType::DeclaredType(func) =
                typecheck_expression(func, outer_scopes, current_scope, context)?
            else {
                return Err(TypecheckError::CantCall(node.provenance.clone()));
            };
            let func = context.id_to_func(&func);

            if func.params.len() != args.len() {
                return Err(TypecheckError::WrongArgsCount(node.provenance.clone()));
            }

            for (arg, param) in args.iter().zip(func.params.iter()) {
                let arg = typecheck_expression(arg, outer_scopes, current_scope, context)?;
                if !is_assignable_to(&param, &arg) {
                    return Err(TypecheckError::TypeMismatch {
                        received: arg.clone(),
                        expected: param.clone(),
                    });
                }
            }

            func.returns.clone()
        }
        AstNodeValue::StructLiteral { name, fields } => {
            let ExpressionType::DeclaredType(struct_type_id) =
                typecheck_expression(name, outer_scopes, current_scope, context)?
            else {
                return Err(TypecheckError::CantCall(node.provenance.clone()));
            };
            let struct_type = context.id_to_struct(&struct_type_id);

            if struct_type.fields.len() != fields.len() {
                return Err(TypecheckError::WrongArgsCount(node.provenance.clone()));
            }

            for (name, param_field) in struct_type.fields.iter() {
                let Some(arg_field) = fields.get(name) else {
                    return Err(TypecheckError::MissingField(node.provenance.clone()));
                };
                let arg_field =
                    typecheck_expression(arg_field, outer_scopes, current_scope, context)?;
                if !is_assignable_to(param_field, &arg_field) {
                    return Err(TypecheckError::TypeMismatch {
                        received: arg_field.clone(),
                        expected: param_field.clone(),
                    });
                }
            }

            ExpressionType::DeclaredType(*struct_type_id)
        }
        AstNodeValue::DictLiteral(_entries) => todo!(),
        // TODO: assert that this is an lvalue
        AstNodeValue::TakeUnique(inner) => ExpressionType::Pointer(
            PointerKind::Unique,
            Box::new(typecheck_expression(inner, outer_scopes, current_scope, context)?.clone()),
        ),
        AstNodeValue::TakeShared(inner) => ExpressionType::Pointer(
            PointerKind::Shared,
            Box::new(typecheck_expression(inner, outer_scopes, current_scope, context)?.clone()),
        ),
        AstNodeValue::ArrayLiteral(_items) => {
            // TODO: how to determine what the intended array type is?
            todo!();
        }
        AstNodeValue::ArrayLiteralLength(value, _length) => {
            let inner = typecheck_expression(value, outer_scopes, current_scope, context)?;
            ExpressionType::Array(Box::new(inner.clone()))
        }
    };

    node.ty.set(ty).expect("each node should be visited once");

    Ok(node.ty.get().expect("just set"))
}

fn resolve_name(
    name: &str,
    current_scope: &HashMap<String, (ID, ExpressionType)>,
    outer_scopes: &[&HashMap<String, (ID, ExpressionType)>],
) -> Option<(ID, ExpressionType)> {
    current_scope
        .get(name)
        .or(outer_scopes.iter().find_map(|scope| scope.get(name)))
        .cloned()
}

// TODO
fn is_assignable_to(left: &ExpressionType, right: &ExpressionType) -> bool {
    use ExpressionType::*;

    match (left, right) {
        (Pointer(left_ty, left_inner), Pointer(right_ty, right_inner)) => {
            left_ty == right_ty && is_assignable_to(left_inner, right_inner)
        }
        (left, Pointer(_, right_inner)) => is_assignable_to(left, right_inner),
        (Pointer(_, _), _right) => false,
        (Nullable(_), Null) => true,
        (Nullable(left), Nullable(right)) => is_assignable_to(left, right),
        (Nullable(left), right) => is_assignable_to(left, right),
        (left, right) => left == right,
    }
}

fn fully_dereference(ty: &ExpressionType) -> &ExpressionType {
    if let ExpressionType::Pointer(_, inner) = ty {
        fully_dereference(inner)
    } else {
        ty
    }
}
