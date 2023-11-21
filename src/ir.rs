use std::collections::HashMap;

use typed_arena::Arena;

use crate::{
    id::ID,
    parser::{AstNode, AstNodeValue, BinOp, IfDeclaration},
    typecheck::{TypecheckedFile, TypecheckedFunction},
};

// TODO: should the IR be a stack machine?

pub struct IrModule<'a> {
    pub top_level_statements: Vec<IrNode<'a>>,
    // TODO: include imports, structs, and extern function declaration
    pub functions: Vec<IrFunction<'a>>,
}

pub fn lower_module<'ast, 'ir>(
    arena: &'ir Arena<IrNode<'ir>>,
    module: TypecheckedFile<'ast>,
) -> IrModule<'ir> {
    let TypecheckedFile {
        expression_types: _,
        referenced_ids,
        functions,
        top_level_statements,
    } = module;
    IrModule {
        functions: functions
            .into_iter()
            .map(|func| lower_function(arena, &referenced_ids, func))
            .collect(),
        top_level_statements: top_level_statements
            .into_iter()
            .map(|stmt| lower_node(arena, &referenced_ids, &stmt))
            .collect(),
    }
}

fn lower_function<'ast, 'ir>(
    arena: &'ir Arena<IrNode<'ir>>,
    referenced_ids: &HashMap<ID, ID>,
    func: TypecheckedFunction<'ast>,
) -> IrFunction<'ir> {
    let mut instructions: Vec<_> = func
        .func
        .params
        .iter()
        .enumerate()
        .map(|(i, param)| IrNode {
            id: param.id,
            value: IrNodeValue::Parameter(i, param.id),
        })
        .collect();
    instructions.push(lower_node(arena, &referenced_ids, func.func.body));
    IrFunction {
        id: func.id,
        name: func.name,
        body: IrNode {
            id: func.id,
            value: IrNodeValue::Sequence(instructions),
        },
    }
}

// TODO: include types in all IR nodes?

pub struct IrFunction<'a> {
    pub id: ID,
    pub name: String,
    pub body: IrNode<'a>,
}

fn lower_node<'ast, 'ir>(
    arena: &'ir Arena<IrNode<'ir>>,
    referenced_ids: &HashMap<ID, ID>,
    node: &'ast AstNode<'ast>,
) -> IrNode<'ir> {
    let value = match &node.value {
        AstNodeValue::Int(x) => IrNodeValue::Int(*x),
        AstNodeValue::Float(x) => IrNodeValue::Float(*x),
        AstNodeValue::Bool(x) => IrNodeValue::Bool(*x),

        AstNodeValue::While(cond, body) => {
            // TODO: can you assign out of a while?
            let cond = lower_node_alloc(arena, referenced_ids, cond);
            let body = lower_node_alloc(arena, referenced_ids, body);
            IrNodeValue::While(cond, body)
        }
        AstNodeValue::Block(contents) => {
            let contents = contents
                .iter()
                .map(|node| lower_node(arena, referenced_ids, node))
                .collect();

            IrNodeValue::Sequence(contents)
        }
        // TODO
        AstNodeValue::If(IfDeclaration {
            condition,
            if_branch,
            else_branch,
        }) => {
            let condition = lower_node_alloc(arena, referenced_ids, condition);
            let if_branch = lower_node_alloc(arena, referenced_ids, if_branch);
            let else_branch = else_branch
                .as_ref()
                .map(|else_branch| lower_node_alloc(arena, referenced_ids, else_branch));

            IrNodeValue::If(condition, if_branch, else_branch)
        }

        AstNodeValue::TakeUnique(inner) => {
            IrNodeValue::TakeUnique(lower_node_alloc(arena, referenced_ids, inner))
        }
        AstNodeValue::TakeShared(inner) => {
            IrNodeValue::Return(lower_node_alloc(arena, referenced_ids, inner))
        }

        // Statement doesn't actually add a node - the inner expression
        // has what really counts
        AstNodeValue::Statement(inner) => return lower_node(arena, referenced_ids, inner),

        AstNodeValue::Declaration(_lvalue, rvalue) => {
            let lvalue = add_node(
                arena,
                IrNode::from_ast(node, IrNodeValue::VariableReference(node.id)),
            );
            let rvalue = lower_node_alloc(arena, referenced_ids, rvalue);
            let statements = vec![
                IrNode::from_ast(node, IrNodeValue::Declaration(node.id)),
                IrNode::from_ast(node, IrNodeValue::Assignment(lvalue, rvalue)),
            ];
            IrNodeValue::Sequence(statements)
        }
        AstNodeValue::Name(_) => IrNodeValue::VariableReference(
            *referenced_ids
                .get(&node.id)
                .expect("referenced ID to be filled in"),
        ),

        AstNodeValue::Return(inner) => {
            return lower_node(arena, referenced_ids, inner);
        }
        AstNodeValue::BinExpr(BinOp::Dot, left, right) => {
            let left = lower_node_alloc(arena, referenced_ids, left);
            let AstNodeValue::Name(name) = &right.value else {
                unreachable!()
            };
            IrNodeValue::Dot(left, name.clone())
        }
        AstNodeValue::BinExpr(op, left, right) => {
            let left = lower_node_alloc(arena, referenced_ids, left);
            let right = lower_node_alloc(arena, referenced_ids, right);

            match op {
                BinOp::AddAssign => IrNodeValue::Assignment(
                    left,
                    add_node(
                        arena,
                        IrNode::from_ast(&node, IrNodeValue::BinOp(IrBinOp::Add, left, right)),
                    ),
                ),
                BinOp::SubtractAssign => IrNodeValue::Assignment(
                    left,
                    add_node(
                        arena,
                        IrNode::from_ast(&node, IrNodeValue::BinOp(IrBinOp::Subtract, left, right)),
                    ),
                ),
                BinOp::MultiplyAssign => IrNodeValue::Assignment(
                    left,
                    add_node(
                        arena,
                        IrNode::from_ast(&node, IrNodeValue::BinOp(IrBinOp::Multiply, left, right)),
                    ),
                ),
                BinOp::DivideAssign => IrNodeValue::Assignment(
                    left,
                    add_node(
                        arena,
                        IrNode::from_ast(&node, IrNodeValue::BinOp(IrBinOp::Divide, left, right)),
                    ),
                ),
                BinOp::Assignment => IrNodeValue::Assignment(left, right),

                BinOp::Add => IrNodeValue::BinOp(IrBinOp::Add, left, right),
                BinOp::Subtract => IrNodeValue::BinOp(IrBinOp::Subtract, left, right),
                BinOp::Multiply => IrNodeValue::BinOp(IrBinOp::Multiply, left, right),
                BinOp::Divide => IrNodeValue::BinOp(IrBinOp::Divide, left, right),
                BinOp::LessThan => IrNodeValue::BinOp(IrBinOp::LessThan, left, right),
                BinOp::GreaterThan => IrNodeValue::BinOp(IrBinOp::GreaterThan, left, right),
                BinOp::LessEqualThan => IrNodeValue::BinOp(IrBinOp::LessEqualThan, left, right),
                BinOp::GreaterEqualThan => {
                    IrNodeValue::BinOp(IrBinOp::GreaterEqualThan, left, right)
                }
                BinOp::EqualTo => IrNodeValue::BinOp(IrBinOp::EqualTo, left, right),
                BinOp::NotEquals => IrNodeValue::BinOp(IrBinOp::NotEquals, left, right),
                BinOp::Index => IrNodeValue::Index(left, right),
                BinOp::Dot => unreachable!(),
            }
        }
        AstNodeValue::Call(func, params) => {
            let func = lower_node_alloc(arena, referenced_ids, func);
            let params = params
                .iter()
                .map(|param| lower_node(arena, referenced_ids, param))
                .collect();
            IrNodeValue::Call(func, params)
        }
        AstNodeValue::StructLiteral { name: _, fields } => {
            let id = referenced_ids
                .get(&node.id)
                .expect("referenced fields to be filled in");
            let fields = fields
                .iter()
                .map(|(name, field)| (name.clone(), lower_node(arena, referenced_ids, field)))
                .collect();
            IrNodeValue::StructLiteral(*id, fields)
        }
        AstNodeValue::ArrayLiteral(_) => todo!(),
        AstNodeValue::ArrayLiteralLength(_, _) => todo!(),

        AstNodeValue::FunctionDeclaration(_)
        | AstNodeValue::ExternFunctionBinding(_)
        | AstNodeValue::StructDeclaration(_)
        | AstNodeValue::Import(_)
        | AstNodeValue::UniqueType(_)
        | AstNodeValue::SharedType(_)
        | AstNodeValue::ArrayType(_) => unreachable!("Can't have these in a function body"),
    };

    IrNode::from_ast(node, value)
}

fn lower_node_alloc<'ast, 'ir>(
    arena: &'ir Arena<IrNode<'ir>>,
    context: &HashMap<ID, ID>,
    node: &'ast AstNode<'ast>,
) -> &'ir IrNode<'ir> {
    add_node(arena, lower_node(arena, context, node))
}

fn add_node<'a>(arena: &'a Arena<IrNode<'a>>, node: IrNode<'a>) -> &'a IrNode<'a> {
    arena.alloc(node)
}

pub struct IrNode<'a> {
    pub id: ID,
    pub value: IrNodeValue<'a>,
}

impl<'a> IrNode<'a> {
    pub fn from_ast(ast: &AstNode<'_>, value: IrNodeValue<'a>) -> IrNode<'a> {
        IrNode { id: ast.id, value }
    }
}

// TODO: should struct fields also be referred to via opaque IDs?

pub enum IrNodeValue<'a> {
    /// Give the Nth parameter the given ID
    Parameter(usize, ID),
    VariableReference(ID),
    Declaration(ID),
    // TODO: insert destructors
    Destructor(ID),

    Call(&'a IrNode<'a>, Vec<IrNode<'a>>),
    Dot(&'a IrNode<'a>, String),
    Assignment(&'a IrNode<'a>, &'a IrNode<'a>),
    Index(&'a IrNode<'a>, &'a IrNode<'a>),
    BinOp(IrBinOp, &'a IrNode<'a>, &'a IrNode<'a>),

    Return(&'a IrNode<'a>),

    Int(i64),
    Float(f64),
    Bool(bool),

    TakeUnique(&'a IrNode<'a>),
    TakeShared(&'a IrNode<'a>),
    Dereference(&'a IrNode<'a>),

    /// Like a Block in that it's a collection of nodes, but the IR
    /// doesn't care about scoping or expressions
    Sequence(Vec<IrNode<'a>>),

    // Expressions
    If(&'a IrNode<'a>, &'a IrNode<'a>, Option<&'a IrNode<'a>>),
    While(&'a IrNode<'a>, &'a IrNode<'a>),
    StructLiteral(ID, HashMap<String, IrNode<'a>>),
    ArrayLiteral(Vec<IrNode<'a>>),
    ArrayLiteralLength(&'a IrNode<'a>, u64),
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