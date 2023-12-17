use std::collections::HashMap;

use crate::{
    id::ID,
    parser::AstNode,
    provenance::SourceRange,
    typecheck::{ExpressionType, StaticDeclaration, TypecheckedFile},
};

mod auto_deref_dot;
mod interface_conversion_pass;
mod lower;
mod rewrite_associated_functions;

pub fn lower_module<'ast>(
    module: TypecheckedFile<'ast>,
    declarations: &HashMap<ID, &'ast StaticDeclaration>,
) -> HirModule {
    let mut module = lower::lower_module(module, declarations);

    module.visit_mut(|expr: &mut _| rewrite_associated_functions::rewrite(declarations, expr));
    interface_conversion_pass::rewrite(&mut module, declarations);
    auto_deref_dot::auto_deref_dot(&mut module);

    module
}

// TODO: should the IR be a stack machine?

pub struct HirModule {
    pub top_level_statements: Vec<HirNode>,
    // TODO: include imports, structs, and extern function declaration
    pub functions: Vec<HirFunction>,
}

impl HirModule {
    pub fn visit_mut(&mut self, mut callback: impl FnMut(&mut HirNode)) {
        for expr in self.top_level_statements.iter_mut() {
            expr.visit_mut_recursive(&mut callback);
        }
        for func in self.functions.iter_mut() {
            func.body.visit_mut_recursive(&mut callback);
        }
    }

    pub fn visit(&self, mut callback: impl FnMut(Option<&HirNode>, &HirNode)) {
        for expr in self.top_level_statements.iter() {
            expr.visit_recursive(None, &mut callback);
        }
        for func in self.functions.iter() {
            func.body.visit_recursive(None, &mut callback);
        }
    }
}

#[derive(Clone, Debug)]
pub struct HirFunction {
    pub id: ID,
    pub name: String,
    pub body: HirNode,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HirNode {
    pub id: ID,
    pub value: HirNodeValue,
    pub ty: ExpressionType,
    pub provenance: Option<SourceRange>,
}

impl HirNode {
    pub fn dummy() -> HirNode {
        HirNode {
            id: ID::dummy(),
            value: HirNodeValue::Null,
            ty: ExpressionType::Null,
            provenance: None,
        }
    }

    pub fn autogenerated(value: HirNodeValue, ty: ExpressionType) -> HirNode {
        HirNode {
            id: ID::new(),
            value,
            ty,
            provenance: None,
        }
    }

    pub fn generated_with_id(id: ID, value: HirNodeValue, ty: ExpressionType) -> HirNode {
        HirNode {
            id,
            value,
            ty,
            provenance: None,
        }
    }

    pub fn from_ast(ast: &AstNode<'_>, value: HirNodeValue, ty: ExpressionType) -> HirNode {
        HirNode {
            id: ast.id,
            value,
            ty,
            provenance: Some(ast.provenance.clone()),
        }
    }

    pub fn from_ast_void(ast: &AstNode<'_>, value: HirNodeValue) -> HirNode {
        Self::from_ast(ast, value, ExpressionType::Void)
    }

    pub fn visit_mut(&mut self, mut callback: impl FnMut(&mut HirNode)) {
        self.visit_mut_recursive(&mut callback);
    }

    fn visit_mut_recursive(&mut self, callback: &mut impl FnMut(&mut HirNode)) {
        callback(self);
        match &mut self.value {
            HirNodeValue::Parameter(_, _)
            | HirNodeValue::VariableReference(_)
            | HirNodeValue::Declaration(_)
            | HirNodeValue::Int(_)
            | HirNodeValue::Float(_)
            | HirNodeValue::Bool(_)
            | HirNodeValue::CharLiteral(_)
            | HirNodeValue::StringLiteral(_)
            | HirNodeValue::Null => {}
            HirNodeValue::Call(lhs, params) | HirNodeValue::VtableCall(lhs, _, params) => {
                lhs.visit_mut_recursive(callback);
                for param in params.iter_mut() {
                    param.visit_mut_recursive(callback);
                }
            }
            HirNodeValue::Access(child, _)
            | HirNodeValue::InterfaceAddress(child)
            | HirNodeValue::TakeUnique(child)
            | HirNodeValue::TakeShared(child)
            | HirNodeValue::Dereference(child)
            | HirNodeValue::ArrayLiteralLength(child, _)
            | HirNodeValue::Return(child)
            | HirNodeValue::StructToInterface { value: child, .. } => {
                child.visit_mut_recursive(callback);
            }
            HirNodeValue::Assignment(lhs, rhs)
            | HirNodeValue::Index(lhs, rhs)
            | HirNodeValue::While(lhs, rhs)
            | HirNodeValue::BinOp(_, lhs, rhs) => {
                lhs.visit_mut_recursive(callback);
                rhs.visit_mut_recursive(callback);
            }
            HirNodeValue::Sequence(children) | HirNodeValue::ArrayLiteral(children) => {
                for child in children.iter_mut() {
                    child.visit_mut_recursive(callback);
                }
            }
            HirNodeValue::If(cond, if_branch, else_branch) => {
                cond.visit_mut_recursive(callback);
                if_branch.visit_mut_recursive(callback);
                if let Some(else_branch) = else_branch {
                    else_branch.visit_mut_recursive(callback);
                }
            }
            HirNodeValue::StructLiteral(_, fields) => {
                for field in fields.values_mut() {
                    field.visit_mut_recursive(callback);
                }
            }
        }
    }

    pub fn visit(&self, mut callback: impl FnMut(Option<&HirNode>, &HirNode)) {
        self.visit_recursive(None, &mut callback);
    }

    fn visit_recursive(
        &self,
        parent: Option<&HirNode>,
        callback: &mut impl FnMut(Option<&HirNode>, &HirNode),
    ) {
        callback(parent, self);
        match &self.value {
            HirNodeValue::Parameter(_, _)
            | HirNodeValue::VariableReference(_)
            | HirNodeValue::Declaration(_)
            | HirNodeValue::Int(_)
            | HirNodeValue::Float(_)
            | HirNodeValue::Bool(_)
            | HirNodeValue::CharLiteral(_)
            | HirNodeValue::StringLiteral(_)
            | HirNodeValue::Null => {}
            HirNodeValue::Call(lhs, params) | HirNodeValue::VtableCall(lhs, _, params) => {
                lhs.visit_recursive(Some(self), callback);
                for param in params.iter() {
                    param.visit_recursive(Some(self), callback);
                }
            }
            HirNodeValue::Access(child, _)
            | HirNodeValue::InterfaceAddress(child)
            | HirNodeValue::TakeUnique(child)
            | HirNodeValue::TakeShared(child)
            | HirNodeValue::Dereference(child)
            | HirNodeValue::ArrayLiteralLength(child, _)
            | HirNodeValue::Return(child)
            | HirNodeValue::StructToInterface { value: child, .. } => {
                child.visit_recursive(Some(self), callback);
            }
            HirNodeValue::Assignment(lhs, rhs)
            | HirNodeValue::Index(lhs, rhs)
            | HirNodeValue::While(lhs, rhs)
            | HirNodeValue::BinOp(_, lhs, rhs) => {
                lhs.visit_recursive(Some(self), callback);
                rhs.visit_recursive(Some(self), callback);
            }
            HirNodeValue::Sequence(children) | HirNodeValue::ArrayLiteral(children) => {
                for child in children.iter() {
                    child.visit_recursive(Some(self), callback);
                }
            }
            HirNodeValue::If(cond, if_branch, else_branch) => {
                cond.visit_recursive(Some(self), callback);
                if_branch.visit_recursive(Some(self), callback);
                if let Some(else_branch) = else_branch {
                    else_branch.visit_recursive(Some(self), callback);
                }
            }
            HirNodeValue::StructLiteral(_, fields) => {
                for field in fields.values() {
                    field.visit_recursive(Some(self), callback);
                }
            }
        }
    }
}

// TODO: should struct fields also be referred to via opaque IDs?

#[derive(Clone, Debug, PartialEq)]
pub enum HirNodeValue {
    /// Give the Nth parameter the given ID
    Parameter(usize, ID),
    VariableReference(ID),
    Declaration(ID),

    Call(Box<HirNode>, Vec<HirNode>),
    Access(Box<HirNode>, String),
    Assignment(Box<HirNode>, Box<HirNode>),
    Index(Box<HirNode>, Box<HirNode>),
    BinOp(HirBinOp, Box<HirNode>, Box<HirNode>),

    Return(Box<HirNode>),

    Int(i64),
    Float(f64),
    Bool(bool),
    Null,
    CharLiteral(char),
    StringLiteral(String),

    TakeUnique(Box<HirNode>),
    TakeShared(Box<HirNode>),
    Dereference(Box<HirNode>),

    /// Like a Block in that it's a collection of nodes, but the IR
    /// doesn't care about scoping or expressions
    Sequence(Vec<HirNode>),

    // Expressions
    If(Box<HirNode>, Box<HirNode>, Option<Box<HirNode>>),
    While(Box<HirNode>, Box<HirNode>),
    StructLiteral(ID, HashMap<String, HirNode>),
    ArrayLiteral(Vec<HirNode>),
    ArrayLiteralLength(Box<HirNode>, Box<HirNode>),

    // Instructions only generated by IR passes
    VtableCall(Box<HirNode>, ID, Vec<HirNode>),
    InterfaceAddress(Box<HirNode>),
    StructToInterface {
        value: Box<HirNode>,
        vtable: HashMap<ID, ID>,
    },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum HirBinOp {
    Add,
    Subtract,
    Multiply,
    Divide,
    LessThan,
    GreaterThan,
    LessEqualThan,
    GreaterEqualThan,
    EqualTo,
    NotEquals,
}
