use std::collections::HashMap;

use crate::{
    id::ID,
    parser::AstNode,
    provenance::SourceRange,
    typecheck::{ExpressionType, StaticDeclaration, TypecheckedFile},
};

mod interface_conversion_pass;
mod lower;
mod rewrite_associated_functions;

pub fn lower_module<'ast>(
    module: TypecheckedFile<'ast>,
    declarations: &HashMap<ID, &'ast StaticDeclaration>,
) -> IrModule {
    let mut module = lower::lower_module(module, declarations);

    module.visit_mut(|expr: &mut _| rewrite_associated_functions::rewrite(declarations, expr));
    interface_conversion_pass::rewrite(&mut module, declarations);

    module
}

// TODO: should the IR be a stack machine?

pub struct IrModule {
    pub top_level_statements: Vec<IrNode>,
    // TODO: include imports, structs, and extern function declaration
    pub functions: Vec<IrFunction>,
}

impl IrModule {
    pub fn visit_mut(&mut self, mut callback: impl FnMut(&mut IrNode)) {
        for expr in self.top_level_statements.iter_mut() {
            expr.visit_mut_recursive(&mut callback);
        }
        for func in self.functions.iter_mut() {
            func.body.visit_mut_recursive(&mut callback);
        }
    }

    pub fn visit(&self, mut callback: impl FnMut(Option<&IrNode>, &IrNode)) {
        for expr in self.top_level_statements.iter() {
            expr.visit_recursive(None, &mut callback);
        }
        for func in self.functions.iter() {
            func.body.visit_recursive(None, &mut callback);
        }
    }
}

#[derive(Clone, Debug)]
pub struct IrFunction {
    pub id: ID,
    pub name: String,
    pub body: IrNode,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IrNode {
    pub id: ID,
    pub value: IrNodeValue,
    pub ty: ExpressionType,
    pub provenance: Option<SourceRange>,
}

impl IrNode {
    pub fn dummy() -> IrNode {
        IrNode {
            id: ID::dummy(),
            value: IrNodeValue::Null,
            ty: ExpressionType::Null,
            provenance: None,
        }
    }

    pub fn autogenerated(value: IrNodeValue, ty: ExpressionType) -> IrNode {
        IrNode {
            id: ID::new(),
            value,
            ty,
            provenance: None,
        }
    }

    pub fn generated_with_id(id: ID, value: IrNodeValue, ty: ExpressionType) -> IrNode {
        IrNode {
            id,
            value,
            ty,
            provenance: None,
        }
    }

    pub fn from_ast(ast: &AstNode<'_>, value: IrNodeValue, ty: ExpressionType) -> IrNode {
        IrNode {
            id: ast.id,
            value,
            ty,
            provenance: Some(ast.provenance.clone()),
        }
    }

    pub fn from_ast_void(ast: &AstNode<'_>, value: IrNodeValue) -> IrNode {
        Self::from_ast(ast, value, ExpressionType::Void)
    }

    pub fn visit_mut(&mut self, mut callback: impl FnMut(&mut IrNode)) {
        self.visit_mut_recursive(&mut callback);
    }

    fn visit_mut_recursive(&mut self, callback: &mut impl FnMut(&mut IrNode)) {
        callback(self);
        match &mut self.value {
            IrNodeValue::Parameter(_, _)
            | IrNodeValue::VariableReference(_)
            | IrNodeValue::Declaration(_)
            | IrNodeValue::Int(_)
            | IrNodeValue::Float(_)
            | IrNodeValue::Bool(_)
            | IrNodeValue::CharLiteral(_)
            | IrNodeValue::StringLiteral(_)
            | IrNodeValue::Null => {}
            IrNodeValue::Call(lhs, params) | IrNodeValue::VtableCall(lhs, _, params) => {
                lhs.visit_mut_recursive(callback);
                for param in params.iter_mut() {
                    param.visit_mut_recursive(callback);
                }
            }
            IrNodeValue::Access(child, _)
            | IrNodeValue::TakeUnique(child)
            | IrNodeValue::TakeShared(child)
            | IrNodeValue::Dereference(child)
            | IrNodeValue::ArrayLiteralLength(child, _)
            | IrNodeValue::Return(child)
            | IrNodeValue::StructToInterface { value: child, .. } => {
                child.visit_mut_recursive(callback);
            }
            IrNodeValue::Assignment(lhs, rhs)
            | IrNodeValue::Index(lhs, rhs)
            | IrNodeValue::While(lhs, rhs)
            | IrNodeValue::BinOp(_, lhs, rhs) => {
                lhs.visit_mut_recursive(callback);
                rhs.visit_mut_recursive(callback);
            }
            IrNodeValue::Sequence(children) | IrNodeValue::ArrayLiteral(children) => {
                for child in children.iter_mut() {
                    child.visit_mut_recursive(callback);
                }
            }
            IrNodeValue::If(cond, if_branch, else_branch) => {
                cond.visit_mut_recursive(callback);
                if_branch.visit_mut_recursive(callback);
                if let Some(else_branch) = else_branch {
                    else_branch.visit_mut_recursive(callback);
                }
            }
            IrNodeValue::StructLiteral(_, fields) => {
                for field in fields.values_mut() {
                    field.visit_mut_recursive(callback);
                }
            }
        }
    }

    pub fn visit(&self, mut callback: impl FnMut(Option<&IrNode>, &IrNode)) {
        self.visit_recursive(None, &mut callback);
    }

    fn visit_recursive(
        &self,
        parent: Option<&IrNode>,
        callback: &mut impl FnMut(Option<&IrNode>, &IrNode),
    ) {
        callback(parent, self);
        match &self.value {
            IrNodeValue::Parameter(_, _)
            | IrNodeValue::VariableReference(_)
            | IrNodeValue::Declaration(_)
            | IrNodeValue::Int(_)
            | IrNodeValue::Float(_)
            | IrNodeValue::Bool(_)
            | IrNodeValue::CharLiteral(_)
            | IrNodeValue::StringLiteral(_)
            | IrNodeValue::Null => {}
            IrNodeValue::Call(lhs, params) | IrNodeValue::VtableCall(lhs, _, params) => {
                lhs.visit_recursive(Some(self), callback);
                for param in params.iter() {
                    param.visit_recursive(Some(self), callback);
                }
            }
            IrNodeValue::Access(child, _)
            | IrNodeValue::TakeUnique(child)
            | IrNodeValue::TakeShared(child)
            | IrNodeValue::Dereference(child)
            | IrNodeValue::ArrayLiteralLength(child, _)
            | IrNodeValue::Return(child)
            | IrNodeValue::StructToInterface { value: child, .. } => {
                child.visit_recursive(Some(self), callback);
            }
            IrNodeValue::Assignment(lhs, rhs)
            | IrNodeValue::Index(lhs, rhs)
            | IrNodeValue::While(lhs, rhs)
            | IrNodeValue::BinOp(_, lhs, rhs) => {
                lhs.visit_recursive(Some(self), callback);
                rhs.visit_recursive(Some(self), callback);
            }
            IrNodeValue::Sequence(children) | IrNodeValue::ArrayLiteral(children) => {
                for child in children.iter() {
                    child.visit_recursive(Some(self), callback);
                }
            }
            IrNodeValue::If(cond, if_branch, else_branch) => {
                cond.visit_recursive(Some(self), callback);
                if_branch.visit_recursive(Some(self), callback);
                if let Some(else_branch) = else_branch {
                    else_branch.visit_recursive(Some(self), callback);
                }
            }
            IrNodeValue::StructLiteral(_, fields) => {
                for field in fields.values() {
                    field.visit_recursive(Some(self), callback);
                }
            }
        }
    }
}

// TODO: should struct fields also be referred to via opaque IDs?

#[derive(Clone, Debug, PartialEq)]
pub enum IrNodeValue {
    /// Give the Nth parameter the given ID
    Parameter(usize, ID),
    VariableReference(ID),
    Declaration(ID),

    Call(Box<IrNode>, Vec<IrNode>),
    Access(Box<IrNode>, String),
    Assignment(Box<IrNode>, Box<IrNode>),
    Index(Box<IrNode>, Box<IrNode>),
    BinOp(IrBinOp, Box<IrNode>, Box<IrNode>),

    Return(Box<IrNode>),

    Int(i64),
    Float(f64),
    Bool(bool),
    Null,
    CharLiteral(char),
    StringLiteral(String),

    TakeUnique(Box<IrNode>),
    TakeShared(Box<IrNode>),
    Dereference(Box<IrNode>),

    /// Like a Block in that it's a collection of nodes, but the IR
    /// doesn't care about scoping or expressions
    Sequence(Vec<IrNode>),

    // Expressions
    If(Box<IrNode>, Box<IrNode>, Option<Box<IrNode>>),
    While(Box<IrNode>, Box<IrNode>),
    StructLiteral(ID, HashMap<String, IrNode>),
    ArrayLiteral(Vec<IrNode>),
    ArrayLiteralLength(Box<IrNode>, Box<IrNode>),

    // Instructions only generated by IR passes
    VtableCall(Box<IrNode>, ID, Vec<IrNode>),
    StructToInterface {
        value: Box<IrNode>,
        vtable: HashMap<ID, ID>,
    },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum IrBinOp {
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

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum IrLvalueBinOp {
    Index,
    Assignment,
}
