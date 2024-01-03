use std::collections::{HashMap, VecDeque};

use crate::{
    hir::{HirBinOp, HirFunction, HirNode, HirNodeValue},
    id::{FunctionID, TypeID, VariableID},
    provenance::SourceRange,
    typecheck::{CollectionType, ExpressionType, PrimitiveType, StaticDeclaration},
};

#[derive(Debug)]
pub struct LinearFunction {
    pub id: FunctionID,
    // TODO: memory layouts instead of expression types?
    pub params: Vec<ExpressionType>,
    pub returns: Option<ExpressionType>,
    pub body: Vec<LinearNode>,
}

pub fn linearize_function(
    declarations: &HashMap<TypeID, TypeMemoryLayout>,
    function: HirFunction,
) -> LinearFunction {
    let HirNodeValue::Sequence(block) = function.body.value else {
        unreachable!()
    };
    let body = linearize_nodes(
        declarations,
        &mut HashMap::new(),
        &mut std::mem::size_of::<usize>(),
        block.into(),
    );
    LinearFunction {
        id: function.id,
        body,
        // TODO
        params: Vec::new(),
        returns: None,
    }
}

#[derive(Clone, Debug)]
pub struct LinearNode {
    pub value: LinearNodeValue,
    pub provenance: Option<SourceRange>,
}

impl LinearNode {
    pub fn new(value: LinearNodeValue) -> LinearNode {
        LinearNode {
            value,
            provenance: None,
        }
    }

    fn if_node(
        cond: LinearNode,
        if_block: Vec<LinearNode>,
        else_block: Option<Vec<LinearNode>>,
        provenance: Option<SourceRange>,
    ) -> LinearNode {
        LinearNode {
            value: LinearNodeValue::If(Box::new(cond), if_block, else_block),
            provenance,
        }
    }

    fn ptr_op(op: HirBinOp, lhs: LinearNode, rhs: LinearNode) -> LinearNode {
        LinearNode {
            value: LinearNodeValue::BinOp(
                op,
                PrimitiveType::PointerSize,
                Box::new(lhs),
                Box::new(rhs),
            ),
            provenance: None,
        }
    }

    fn size(size: usize) -> LinearNode {
        LinearNode {
            value: LinearNodeValue::Size(size),
            provenance: None,
        }
    }

    fn heap_alloc(size: usize) -> LinearNode {
        LinearNode {
            value: LinearNodeValue::HeapAlloc(Box::new(LinearNode::size(size))),
            provenance: None,
        }
    }

    fn write_temp(tmp: u8, value: LinearNode) -> LinearNode {
        LinearNode {
            value: LinearNodeValue::WriteTemporary(tmp, Box::new(value)),
            provenance: None,
        }
    }

    fn read_temp(tmp: u8) -> LinearNode {
        LinearNode {
            value: LinearNodeValue::ReadTemporary(tmp),
            provenance: None,
        }
    }

    fn read_memory(location: LinearNode, offset: usize, ty: ExpressionType) -> LinearNode {
        LinearNode {
            value: LinearNodeValue::ReadMemory {
                location: Box::new(location),
                offset,
                ty,
            },
            provenance: None,
        }
    }

    fn write_memory(
        location: LinearNode,
        offset: usize,
        ty: ExpressionType,
        value: LinearNode,
    ) -> LinearNode {
        LinearNode {
            value: LinearNodeValue::WriteMemory {
                location: Box::new(location),
                offset,
                ty,
                value: Box::new(value),
            },
            provenance: None,
        }
    }

    fn abort() -> LinearNode {
        LinearNode {
            value: LinearNodeValue::Abort,
            provenance: None,
        }
    }
}

// TODO: split up between 'statement' and 'expression' to reduce need for boxing?
#[derive(Clone, Debug)]
pub enum LinearNodeValue {
    // TODO: how to handle function parameters?

    // Memory
    StackFrame,
    StackAlloc(usize),
    /// Returns the heap pointer
    HeapAlloc(Box<LinearNode>),
    /// Each parameter may only appear once in a given method body
    Parameter(usize),
    ReadMemory {
        location: Box<LinearNode>,
        offset: usize,
        ty: ExpressionType,
    },
    WriteMemory {
        location: Box<LinearNode>,
        offset: usize,
        ty: ExpressionType,
        value: Box<LinearNode>,
    },

    // TODO: just full stack machine?
    TopOfStack,
    Discard,

    // Control flow
    Call(Box<LinearNode>, Vec<LinearNode>),
    Return(Box<LinearNode>),
    If(Box<LinearNode>, Vec<LinearNode>, Option<Vec<LinearNode>>),
    // TODO: labelled breaks?
    Break,
    Loop(Vec<LinearNode>),
    // TODO: stack unwind?
    Abort,

    Sequence(Vec<LinearNode>),
    /*
     * Temporaries are bound to specific sequences
     */
    WriteTemporary(u8, Box<LinearNode>),
    ReadTemporary(u8),

    BinOp(HirBinOp, PrimitiveType, Box<LinearNode>, Box<LinearNode>),
    Cast {
        value: Box<LinearNode>,
        from: PrimitiveType,
        to: PrimitiveType,
    },
    Size(usize),
    Int(i64),
    Float(f64),
    Bool(bool),
    Null,
    CharLiteral(char),
    StringLiteral(String),
    FunctionID(FunctionID),
}

// TODO: produce a more CFG shaped result?

pub fn linearize_nodes(
    declarations: &HashMap<TypeID, TypeMemoryLayout>,
    stack_entries: &mut HashMap<VariableID, usize>,
    stack_offset: &mut usize,
    //blocks: &mut VecDeque<LinearBlock>,
    mut nodes: VecDeque<HirNode>,
) -> Vec<LinearNode> {
    let mut values = Vec::new();

    while let Some(node) = nodes.pop_front() {
        match node.value {
            HirNodeValue::Sequence(inner_nodes) => {
                nodes.reserve(inner_nodes.len());
                for node in inner_nodes.into_iter().rev() {
                    nodes.push_front(node);
                }
            }
            HirNodeValue::Declaration(id) => {
                let alloc_size = expression_type_size(declarations, &node.ty);
                *stack_offset += alloc_size;
                stack_entries.insert(id, *stack_offset);
                let alloc = LinearNode::new(LinearNodeValue::StackAlloc(alloc_size));
                values.push(alloc);
            }
            HirNodeValue::Parameter(idx, id) => {
                let alloc_size = expression_type_size(declarations, &node.ty);
                *stack_offset += alloc_size;
                stack_entries.insert(id, *stack_offset);
                let alloc = LinearNode::new(LinearNodeValue::StackAlloc(alloc_size));
                values.push(alloc);

                let (location, offset) = variable_location(stack_entries, id);

                values.push(LinearNode {
                    value: LinearNodeValue::WriteMemory {
                        location: Box::new(location),
                        offset,
                        value: Box::new(LinearNode::new(LinearNodeValue::Parameter(idx))),
                        // TODO: hm
                        ty: node.ty,
                    },
                    provenance: node.provenance,
                });
            }
            HirNodeValue::Assignment(lhs, rhs) => {
                let is_union = if let ExpressionType::InstanceOf(ty) = &lhs.ty {
                    // TODO: this is wrong, we need to detect if the left side is assigning to a
                    // union variant
                    declarations
                        .get(ty)
                        .map(|ty| matches!(&ty.value, TypeLayoutValue::Union(_)))
                        .unwrap_or(false)
                        && false
                } else {
                    false
                };
                if is_union {
                    todo!()
                } else {
                    let ty = lhs.ty.clone();
                    let (location, offset) = lower_lvalue(declarations, stack_entries, *lhs);
                    let rhs = lower_expression(declarations, stack_entries, *rhs);
                    values.push(LinearNode {
                        value: LinearNodeValue::WriteMemory {
                            location: Box::new(location),
                            offset,
                            value: Box::new(rhs),
                            ty,
                        },
                        provenance: node.provenance,
                    });
                }
            }

            HirNodeValue::Return(expr) => {
                let expr = lower_expression(declarations, stack_entries, *expr);
                values.push(LinearNode {
                    value: LinearNodeValue::Return(Box::new(expr)),
                    provenance: node.provenance,
                });
                // TODO: is this a legal optimization?
                break;
            }
            // TODO: should If be an expression in linear IR?
            HirNodeValue::If(cond, if_block, else_block) => {
                let cond = lower_expression(declarations, stack_entries, *cond);
                let HirNodeValue::Sequence(if_block) = if_block.value else {
                    unreachable!()
                };
                let if_block =
                    linearize_nodes(declarations, stack_entries, stack_offset, if_block.into());
                let else_block = else_block.map(|else_block| {
                    let HirNodeValue::Sequence(else_block) = else_block.value else {
                        unreachable!()
                    };
                    linearize_nodes(declarations, stack_entries, stack_offset, else_block.into())
                });
                values.push(LinearNode::if_node(
                    cond,
                    if_block,
                    else_block,
                    node.provenance,
                ));
            }
            HirNodeValue::While(cond, block) => {
                let cond = lower_expression(declarations, stack_entries, *cond);
                let mut vec_deque = VecDeque::new();
                vec_deque.push_back(*block);
                let block = linearize_nodes(declarations, stack_entries, stack_offset, vec_deque);
                values.push(LinearNode {
                    value: LinearNodeValue::Loop(vec![LinearNode::if_node(
                        cond,
                        block,
                        Some(vec![LinearNode::new(LinearNodeValue::Break)]),
                        None,
                    )]),
                    provenance: node.provenance,
                })
            }

            // TODO: auto-returns?
            _ => {
                values.push(lower_expression(declarations, stack_entries, node));
            }
        }
    }

    values
}

fn lower_expression(
    declarations: &HashMap<TypeID, TypeMemoryLayout>,
    stack_entries: &HashMap<VariableID, usize>,
    expression: HirNode,
) -> LinearNode {
    let HirNode {
        id: _,
        value,
        ty,
        provenance,
    } = expression;
    let value = match value {
        HirNodeValue::Int(x) => LinearNodeValue::Int(x),
        HirNodeValue::Float(x) => LinearNodeValue::Float(x),
        HirNodeValue::Bool(x) => LinearNodeValue::Bool(x),
        HirNodeValue::Null => LinearNodeValue::Null,
        HirNodeValue::CharLiteral(x) => LinearNodeValue::CharLiteral(x),
        HirNodeValue::StringLiteral(x) => LinearNodeValue::StringLiteral(x),

        HirNodeValue::BinOp(op, lhs, rhs) => {
            let ExpressionType::Primitive(ty) = rhs.ty else {
                unreachable!("binoperands must be primitive not {:?}", ty)
            };
            LinearNodeValue::BinOp(
                op,
                ty,
                Box::new(lower_expression(declarations, stack_entries, *lhs)),
                Box::new(lower_expression(declarations, stack_entries, *rhs)),
            )
        }
        HirNodeValue::VariableReference(id) => {
            let (location, offset) = variable_location(stack_entries, id.as_var());
            LinearNodeValue::ReadMemory {
                location: Box::new(location),
                offset,
                ty,
            }
        }
        HirNodeValue::Call(lhs, params) => {
            let HirNodeValue::VariableReference(fn_id) = lhs.value else {
                unreachable!("lhs of function call must be a function ID")
            };
            let params = params
                .into_iter()
                .map(|param| lower_expression(declarations, stack_entries, param))
                .collect();
            LinearNodeValue::Call(
                Box::new(LinearNode::new(LinearNodeValue::FunctionID(fn_id.as_fn()))),
                params,
            )
        }
        HirNodeValue::Access(lhs, rhs) => {
            if let Some(ty) = lhs.ty.id().and_then(|id| match &declarations[id].value {
                TypeLayoutValue::Union(ty) => Some(ty),
                _ => None,
            }) {
                let (variant_idx, variant_ty) = &ty[&rhs];
                let (location, offset) = lower_lvalue(declarations, stack_entries, *lhs);
                // TODO: switch from expression type to layout type
                let variant_ty = match variant_ty {
                    TypeLayoutField::Primitive(p) => ExpressionType::Primitive(*p),
                    TypeLayoutField::Referenced(ty) => ExpressionType::InstanceOf(*ty),
                    TypeLayoutField::Nullable(_) => todo!(),
                    TypeLayoutField::Pointer => todo!(),
                };
                LinearNodeValue::Sequence(vec![LinearNode::if_node(
                    LinearNode::ptr_op(
                        HirBinOp::NotEquals,
                        LinearNode::size(*variant_idx),
                        LinearNode::read_memory(
                            location.clone(),
                            offset,
                            ExpressionType::Primitive(PrimitiveType::PointerSize),
                        ),
                    ),
                    vec![LinearNode::abort()],
                    Some(vec![LinearNode::read_memory(
                        location,
                        offset + UNION_TAG_SIZE,
                        variant_ty,
                    )]),
                    None,
                )])
            } else {
                let (location, offset) = access_location(declarations, stack_entries, *lhs, rhs);
                LinearNodeValue::ReadMemory {
                    location: Box::new(location),
                    offset,
                    ty,
                }
            }
        }
        HirNodeValue::ArrayIndex(arr, idx) => {
            let (location, offset) =
                array_index_location(declarations, stack_entries, *arr, *idx, &ty);
            LinearNodeValue::ReadMemory {
                location: Box::new(location),
                offset,
                ty,
            }
        }

        HirNodeValue::If(_, _, _) | HirNodeValue::While(_, _) | HirNodeValue::Return(_) => {
            unreachable!("all control flow must be removed from expressions in HIR")
        }

        HirNodeValue::Parameter(_, _) => todo!(),
        HirNodeValue::Declaration(_) => todo!(),
        HirNodeValue::Assignment(_, _) => todo!(),
        HirNodeValue::TakeUnique(inner) | HirNodeValue::TakeShared(inner) => {
            let (ptr, offset) = lower_lvalue(declarations, stack_entries, *inner);
            LinearNodeValue::BinOp(
                HirBinOp::Add,
                PrimitiveType::PointerSize,
                Box::new(ptr),
                Box::new(LinearNode::new(LinearNodeValue::Size(offset))),
            )
        }
        HirNodeValue::Dereference(inner) => LinearNodeValue::ReadMemory {
            location: Box::new(lower_expression(declarations, stack_entries, *inner)),
            offset: 0,
            ty,
        },
        HirNodeValue::Sequence(_) => todo!(),
        HirNodeValue::StructLiteral(struct_id, mut values) => {
            let Some(TypeMemoryLayout {
                value: TypeLayoutValue::Structure(layouts),
                ..
            }) = declarations.get(&struct_id)
            else {
                unreachable!()
            };
            // TODO: push this down a layer of abstraction
            LinearNodeValue::Sequence(
                layouts
                    .iter()
                    .rev()
                    .map(|(key, _, _)| values.remove(key).unwrap())
                    .map(|value| lower_expression(declarations, stack_entries, value))
                    .collect(),
            )
        }
        HirNodeValue::ArrayLiteral(values) => {
            let ExpressionType::Collection(CollectionType::Array(inner_ty)) = ty else {
                unreachable!()
            };
            let size = expression_type_size(declarations, &inner_ty);

            let length = values.len();

            let inner_ty = *inner_ty;
            let mut instrs = vec![LinearNode::write_temp(
                0,
                LinearNode::heap_alloc(size * values.len()),
            )];
            instrs.extend(values.into_iter().enumerate().map(|(idx, value)| {
                LinearNode::new(LinearNodeValue::WriteMemory {
                    location: Box::new(LinearNode::read_temp(0)),
                    offset: size * idx,
                    ty: inner_ty.clone(),
                    value: Box::new(lower_expression(declarations, stack_entries, value)),
                })
            }));
            // capacity
            instrs.push(LinearNode::size(length));
            // length
            instrs.push(LinearNode::size(length));
            instrs.push(LinearNode::new(LinearNodeValue::ReadTemporary(0)));

            LinearNodeValue::Sequence(instrs)
        }
        HirNodeValue::ArrayLiteralLength(value, length) => {
            let ExpressionType::Collection(CollectionType::Array(inner_ty)) = ty else {
                unreachable!()
            };
            let size = expression_type_size(declarations, &inner_ty);
            let length = lower_expression(declarations, stack_entries, *length);
            let inner_ty = *inner_ty;

            LinearNodeValue::Sequence(vec![
                // Store the length of the array in a temporary
                LinearNode::write_temp(0, length),
                // Allocate memory and store the pointer in another temporary
                LinearNode::write_temp(
                    1,
                    LinearNode::new(LinearNodeValue::HeapAlloc(Box::new(LinearNode::ptr_op(
                        HirBinOp::Multiply,
                        LinearNode::size(size),
                        LinearNode::read_temp(0),
                    )))),
                ),
                // Create a temporary for indexing the array as we fill it in
                LinearNode::write_temp(2, LinearNode::size(0)),
                LinearNode::new(LinearNodeValue::Loop(vec![LinearNode::if_node(
                    // idx = length?
                    LinearNode::ptr_op(
                        HirBinOp::EqualTo,
                        LinearNode::read_temp(2),
                        LinearNode::read_temp(0),
                    ),
                    vec![LinearNode::new(LinearNodeValue::Break)],
                    Some(vec![
                        // *(ptr + idx * size) = value
                        LinearNode::write_memory(
                            LinearNode::ptr_op(
                                HirBinOp::Add,
                                LinearNode::read_temp(1),
                                LinearNode::ptr_op(
                                    HirBinOp::Multiply,
                                    LinearNode::size(size),
                                    LinearNode::read_temp(2),
                                ),
                            ),
                            0,
                            inner_ty.clone(),
                            lower_expression(declarations, stack_entries, *value),
                        ),
                        // idx += 1
                        LinearNode::write_temp(
                            2,
                            LinearNode::ptr_op(
                                HirBinOp::Add,
                                LinearNode::read_temp(2),
                                LinearNode::size(1),
                            ),
                        ),
                    ]),
                    None,
                )])),
                LinearNode::read_temp(0),
                LinearNode::read_temp(0),
                LinearNode::read_temp(1),
            ])
        }
        HirNodeValue::InterfaceAddress(table) => {
            let (table, offset) = lower_lvalue(declarations, stack_entries, *table);
            LinearNodeValue::ReadMemory {
                location: Box::new(table),
                offset,
                ty: ExpressionType::Primitive(PrimitiveType::PointerSize),
            }
        }
        HirNodeValue::VtableCall(table, fn_id, params) => {
            let ExpressionType::InstanceOf(ty_id) = &table.ty else {
                unreachable!()
            };
            let Some(TypeMemoryLayout {
                value: TypeLayoutValue::Interface(fields),
                ..
            }) = declarations.get(ty_id)
            else {
                unreachable!()
            };
            let (table, mut offset) = lower_lvalue(declarations, stack_entries, *table);
            offset += POINTER_SIZE;
            offset += fields
                .iter()
                .enumerate()
                .find_map(|(idx, id)| {
                    if *id == fn_id {
                        // TODO: fix this constant
                        Some(idx * 4)
                    } else {
                        None
                    }
                })
                .unwrap();
            let params = params
                .into_iter()
                .map(|param| lower_expression(declarations, stack_entries, param))
                .collect();

            LinearNodeValue::Call(
                Box::new(LinearNode::new(LinearNodeValue::ReadMemory {
                    location: Box::new(table),
                    offset,
                    ty: ExpressionType::Primitive(PrimitiveType::FunctionID),
                })),
                params,
            )
        }
        HirNodeValue::StructToInterface { value, vtable } => {
            let mut values = Vec::new();

            let ExpressionType::InstanceOf(ty_id) = &ty else {
                unreachable!()
            };
            let Some(TypeMemoryLayout {
                value: TypeLayoutValue::Interface(fields),
                ..
            }) = declarations.get(ty_id)
            else {
                unreachable!()
            };
            for field in fields.iter().rev() {
                values.push(LinearNode::new(LinearNodeValue::FunctionID(vtable[field])));
            }

            let (pointer, offset) = lower_lvalue(declarations, stack_entries, *value);
            let pointer = LinearNode::ptr_op(HirBinOp::Add, pointer, LinearNode::size(offset));
            values.push(pointer);

            LinearNodeValue::Sequence(values)
        }
        HirNodeValue::NumericCast { value, from, to } => LinearNodeValue::Cast {
            value: Box::new(lower_expression(declarations, stack_entries, *value)),
            from,
            to,
        },
        HirNodeValue::DictIndex(dict, idx) => {
            let (location, offset) = dict_index_location(declarations, stack_entries, *dict, *idx);
            LinearNodeValue::ReadMemory {
                location: Box::new(location),
                offset,
                ty,
            }
        }
        HirNodeValue::DictLiteral(entries) => {
            let ExpressionType::Collection(CollectionType::Dict(key_ty, value_ty)) = ty else {
                unreachable!()
            };
            let key_ty = *key_ty;
            let value_ty = *value_ty;

            let key_size = expression_type_size(declarations, &key_ty);
            let value_size = expression_type_size(declarations, &value_ty);
            let entry_size = key_size + value_size;

            let length = entries.len();

            let mut instrs = vec![LinearNode::write_temp(
                0,
                LinearNode::heap_alloc(entry_size * length),
            )];
            let (keys, values): (Vec<_>, Vec<_>) = entries.into_iter().unzip();
            instrs.extend(keys.into_iter().enumerate().map(|(idx, value)| {
                LinearNode::new(LinearNodeValue::WriteMemory {
                    location: Box::new(LinearNode::read_temp(0)),
                    offset: entry_size * idx,
                    ty: key_ty.clone(),
                    value: Box::new(lower_expression(declarations, stack_entries, value)),
                })
            }));
            instrs.extend(values.into_iter().enumerate().map(|(idx, value)| {
                LinearNode::new(LinearNodeValue::WriteMemory {
                    location: Box::new(LinearNode::read_temp(0)),
                    offset: entry_size * idx + key_size,
                    ty: value_ty.clone(),
                    value: Box::new(lower_expression(declarations, stack_entries, value)),
                })
            }));
            // capacity
            instrs.push(LinearNode::size(length));
            // length
            instrs.push(LinearNode::size(length));
            instrs.push(LinearNode::new(LinearNodeValue::ReadTemporary(0)));

            LinearNodeValue::Sequence(instrs)
        }
        HirNodeValue::UnionLiteral(ty, variant, value) => {
            let TypeLayoutValue::Union(ty) = &declarations[&ty].value else {
                unreachable!()
            };
            LinearNodeValue::Sequence(vec![
                lower_expression(declarations, stack_entries, *value),
                LinearNode::new(LinearNodeValue::Size(ty[&variant].0)),
            ])
        }
    };

    LinearNode { value, provenance }
}

fn lower_lvalue(
    declarations: &HashMap<TypeID, TypeMemoryLayout>,
    stack_entries: &HashMap<VariableID, usize>,
    lvalue: HirNode,
) -> (LinearNode, usize) {
    match lvalue.value {
        HirNodeValue::VariableReference(id) => variable_location(stack_entries, id.as_var()),
        HirNodeValue::Access(lhs, rhs) => access_location(declarations, stack_entries, *lhs, rhs),
        HirNodeValue::Dereference(inner) => {
            (lower_expression(declarations, stack_entries, *inner), 0)
        }
        HirNodeValue::ArrayIndex(arr, idx) => {
            array_index_location(declarations, stack_entries, *arr, *idx, &lvalue.ty)
        }
        HirNodeValue::DictIndex(dict, idx) => {
            dict_index_location(declarations, stack_entries, *dict, *idx)
        }

        HirNodeValue::Parameter(_, _) => todo!(),
        HirNodeValue::Declaration(_) => todo!(),
        HirNodeValue::Call(_, _) => todo!(),
        HirNodeValue::Assignment(_, _) => todo!(),
        HirNodeValue::BinOp(_, _, _) => todo!(),
        HirNodeValue::Return(_) => todo!(),
        HirNodeValue::Int(_) => todo!(),
        HirNodeValue::Float(_) => todo!(),
        HirNodeValue::Bool(_) => todo!(),
        HirNodeValue::Null => todo!(),
        HirNodeValue::CharLiteral(_) => todo!(),
        HirNodeValue::StringLiteral(_) => todo!(),
        HirNodeValue::TakeUnique(_) => todo!(),
        HirNodeValue::TakeShared(_) => todo!(),
        HirNodeValue::Sequence(_) => todo!(),
        HirNodeValue::If(_, _, _) => todo!(),
        HirNodeValue::While(_, _) => todo!(),
        HirNodeValue::StructLiteral(_, _) => todo!(),
        HirNodeValue::VtableCall(_, _, _) => todo!(),
        HirNodeValue::StructToInterface { .. } => todo!(),
        HirNodeValue::InterfaceAddress(_) => todo!(),

        HirNodeValue::ArrayLiteral(_) | HirNodeValue::ArrayLiteralLength(_, _) => unreachable!(),
        HirNodeValue::NumericCast { .. } => todo!(),
        HirNodeValue::DictLiteral(_) => todo!(),
        HirNodeValue::UnionLiteral(_, _, _) => todo!(),
    }
}

fn variable_location(
    stack_entries: &HashMap<VariableID, usize>,
    var_id: VariableID,
) -> (LinearNode, usize) {
    (
        LinearNode::new(LinearNodeValue::BinOp(
            HirBinOp::Subtract,
            PrimitiveType::PointerSize,
            Box::new(LinearNode::new(LinearNodeValue::StackFrame)),
            Box::new(LinearNode::new(LinearNodeValue::Size(
                stack_entries[&var_id],
            ))),
        )),
        0,
    )
}

fn access_location(
    declarations: &HashMap<TypeID, TypeMemoryLayout>,
    stack_entries: &HashMap<VariableID, usize>,
    lhs: HirNode,
    rhs: String,
) -> (LinearNode, usize) {
    let ExpressionType::InstanceOf(ty_id) = lhs.ty else {
        unreachable!()
    };
    let TypeMemoryLayout { value, .. } = &declarations[&ty_id];
    let (lhs, mut offset) = lower_lvalue(declarations, stack_entries, lhs);
    offset += match value {
        TypeLayoutValue::Structure(fields) => {
            *(fields
                .iter()
                .find_map(|(name, offset, _)| if name == &rhs { Some(offset) } else { None })
                .unwrap())
        }
        TypeLayoutValue::Union(_) => UNION_TAG_SIZE,
        TypeLayoutValue::Interface(_fields) => todo!(), //*fields.get(&rhs).unwrap(),
        TypeLayoutValue::FunctionPointer => unreachable!(),
    };

    (lhs, offset)
}

fn array_index_location(
    declarations: &HashMap<TypeID, TypeMemoryLayout>,
    stack_entries: &HashMap<VariableID, usize>,
    arr: HirNode,
    idx: HirNode,
    ty: &ExpressionType,
) -> (LinearNode, usize) {
    let size = expression_type_size(declarations, ty);
    let idx = lower_expression(declarations, stack_entries, idx);
    let arr = lower_expression(declarations, stack_entries, arr);
    (
        LinearNode::new(LinearNodeValue::Sequence(vec![
            LinearNode::write_temp(3, idx),
            LinearNode::write_temp(
                0,
                LinearNode::ptr_op(
                    HirBinOp::Multiply,
                    LinearNode::size(size),
                    LinearNode::read_temp(3),
                ),
            ),
            // pointer
            LinearNode::write_temp(1, arr),
            // length
            LinearNode::write_temp(2, LinearNode::new(LinearNodeValue::TopOfStack)),
            LinearNode::if_node(
                LinearNode::ptr_op(
                    HirBinOp::GreaterEqualThan,
                    // index
                    LinearNode::read_temp(3),
                    // length
                    LinearNode::read_temp(2),
                ),
                vec![LinearNode::abort()],
                None,
                None,
            ),
            LinearNode::new(LinearNodeValue::Discard),
            LinearNode::ptr_op(
                HirBinOp::Add,
                LinearNode::read_temp(0),
                LinearNode::read_temp(1),
            ),
        ])),
        0,
    )
}

fn dict_index_location(
    declarations: &HashMap<TypeID, TypeMemoryLayout>,
    stack_entries: &HashMap<VariableID, usize>,
    dict: HirNode,
    idx: HirNode,
) -> (LinearNode, usize) {
    let ExpressionType::Collection(CollectionType::Dict(key_ty, value_ty)) = &dict.ty else {
        unreachable!()
    };
    let ExpressionType::Primitive(idx_ty) = &idx.ty else {
        todo!("non-primitive keys for dictionaries")
    };
    let idx_ty = *idx_ty;

    let key_size = expression_type_size(declarations, key_ty);
    let value_size = expression_type_size(declarations, value_ty);
    let entry_size = key_size + value_size;

    let dict = lower_expression(declarations, stack_entries, dict);

    (
        LinearNode::new(LinearNodeValue::Sequence(vec![
            // pointer
            LinearNode::write_temp(1, dict),
            // length
            LinearNode::write_temp(2, LinearNode::new(LinearNodeValue::TopOfStack)),
            // capacity
            LinearNode::new(LinearNodeValue::Discard),
            // index
            LinearNode::write_temp(3, LinearNode::size(0)),
            LinearNode::new(LinearNodeValue::Loop(vec![
                // Check if we've found the key
                LinearNode::if_node(
                    LinearNode::new(LinearNodeValue::BinOp(
                        HirBinOp::EqualTo,
                        idx_ty,
                        Box::new(LinearNode::read_memory(
                            LinearNode::read_temp(1),
                            0,
                            ExpressionType::Primitive(idx_ty),
                        )),
                        // TODO: don't repeatedly call this
                        Box::new(lower_expression(declarations, stack_entries, idx)),
                    )),
                    vec![
                        LinearNode::read_temp(1),
                        LinearNode::new(LinearNodeValue::Break),
                    ],
                    None,
                    None,
                ),
                // Increment the length counter and check if we've overflowed the bounds
                LinearNode::write_temp(
                    3,
                    LinearNode::ptr_op(
                        HirBinOp::Add,
                        LinearNode::read_temp(3),
                        LinearNode::size(1),
                    ),
                ),
                LinearNode::write_temp(
                    1,
                    LinearNode::ptr_op(
                        HirBinOp::Add,
                        LinearNode::read_temp(1),
                        LinearNode::size(entry_size),
                    ),
                ),
                LinearNode::if_node(
                    LinearNode::ptr_op(
                        HirBinOp::EqualTo,
                        LinearNode::read_temp(2),
                        LinearNode::read_temp(3),
                    ),
                    vec![LinearNode::new(LinearNodeValue::Abort)],
                    None,
                    None,
                ),
            ])),
        ])),
        key_size,
    )
}

// TODO: this should probably be determined by the backend...
const POINTER_SIZE: usize = 8;
// TODO: is this a good idea
const UNION_TAG_SIZE: usize = 4;

fn expression_type_size(
    declarations: &HashMap<TypeID, TypeMemoryLayout>,
    expr: &ExpressionType,
) -> usize {
    // TODO: alignment
    match expr {
        ExpressionType::Void => 0,
        ExpressionType::Primitive(prim) => primitive_type_size(*prim),
        ExpressionType::InstanceOf(id) => declarations[id].size(),
        ExpressionType::Pointer(_, _) => POINTER_SIZE,
        ExpressionType::Collection(CollectionType::Array(_)) => POINTER_SIZE * 3,
        ExpressionType::Collection(CollectionType::Dict(..)) => POINTER_SIZE * 3,
        ExpressionType::Null => 1,
        ExpressionType::Nullable(inner) => {
            // TODO: would this be an alignment issue?
            1 + expression_type_size(declarations, inner)
        }
        ExpressionType::ReferenceTo(_) => todo!(),
    }
}

fn primitive_type_size(prim: PrimitiveType) -> usize {
    match prim {
        PrimitiveType::Char => 1,
        PrimitiveType::String => POINTER_SIZE,
        PrimitiveType::Int32 => 4,
        PrimitiveType::Float32 => 4,
        PrimitiveType::Int64 => 8,
        PrimitiveType::Float64 => 8,
        PrimitiveType::Bool => 1,
        PrimitiveType::PointerSize => POINTER_SIZE,
        PrimitiveType::FunctionID => 4,
    }
}

// TODO

// TODO: move to its own module?
#[derive(Debug)]
pub struct TypeMemoryLayout {
    pub value: TypeLayoutValue,
    // TODO: remove field?
    pub size: usize,
}

#[derive(Debug)]
pub enum TypeLayoutValue {
    Structure(Vec<(String, usize, TypeLayoutField)>),
    Interface(Vec<FunctionID>),
    Union(HashMap<String, (usize, TypeLayoutField)>),
    FunctionPointer,
}

#[derive(Debug)]
pub enum TypeLayoutField {
    Primitive(PrimitiveType),
    Referenced(TypeID),
    Nullable(Box<TypeLayoutField>),
    Pointer,
}

impl TypeMemoryLayout {
    fn size(&self) -> usize {
        self.size
    }
}

pub fn layout_types(
    declarations: &HashMap<String, StaticDeclaration>,
    layouts: &mut HashMap<TypeID, TypeMemoryLayout>,
) {
    let declarations: HashMap<_, _> = declarations
        .iter()
        .map(|(_, decl)| (decl.id(), decl))
        .collect();
    for decl in declarations.values() {
        layout_static_decl(&declarations, layouts, decl);
    }
}

fn layout_static_decl(
    declarations: &HashMap<TypeID, &StaticDeclaration>,
    layouts: &mut HashMap<TypeID, TypeMemoryLayout>,
    decl: &StaticDeclaration,
) -> usize {
    if let Some(layout) = layouts.get(&decl.id()) {
        return layout.size;
    }

    let layout = match decl {
        StaticDeclaration::Struct(struct_ty) => {
            let mut size = 0;
            let fields = struct_ty
                .fields
                .iter()
                .map(|(name, field)| {
                    let (field, field_size) = layout_type(declarations, layouts, field);
                    let offset = size;
                    size += field_size;
                    (name.clone(), offset, field)
                })
                .collect();
            TypeMemoryLayout {
                value: TypeLayoutValue::Structure(fields),
                size,
            }
        }
        StaticDeclaration::Func(_) => TypeMemoryLayout {
            value: TypeLayoutValue::FunctionPointer,
            size: 8,
        },
        StaticDeclaration::Interface(interface_ty) => {
            let mut size = POINTER_SIZE;
            let fields = interface_ty
                .associated_functions
                .values()
                .map(|decl| {
                    // TODO: don't hardcode function ID
                    size += 4;
                    let StaticDeclaration::Func(func) = decl else {
                        unreachable!()
                    };
                    func.func_id
                })
                .collect();
            TypeMemoryLayout {
                value: TypeLayoutValue::Interface(fields),
                size,
            }
        }
        StaticDeclaration::Union(union_ty) => {
            let mut largest_variant = 0;
            let variants = union_ty
                .variants
                .iter()
                .enumerate()
                .map(|(idx, (name, ty))| {
                    let (variant, variant_size) = layout_type(declarations, layouts, ty);
                    if variant_size > largest_variant {
                        largest_variant = variant_size;
                    }

                    (name.clone(), (idx, variant))
                })
                .collect();

            TypeMemoryLayout {
                value: TypeLayoutValue::Union(variants),
                size: UNION_TAG_SIZE + largest_variant,
            }
        }
        // Modules are completely compiled out
        StaticDeclaration::Module(module) => {
            layout_types(&module.exports, layouts);
            return 0;
        }
    };
    let size = layout.size;
    layouts.insert(decl.id(), layout);

    size
}

fn layout_type(
    declarations: &HashMap<TypeID, &StaticDeclaration>,
    layouts: &mut HashMap<TypeID, TypeMemoryLayout>,
    ty: &ExpressionType,
) -> (TypeLayoutField, usize) {
    match ty {
        ExpressionType::Void => unreachable!(),
        ExpressionType::Null => unreachable!(),
        ExpressionType::Primitive(p) => {
            let size = primitive_type_size(*p);
            (TypeLayoutField::Primitive(*p), size)
        }
        ExpressionType::InstanceOf(id) => {
            let size = layout_static_decl(declarations, layouts, declarations[id]);
            (TypeLayoutField::Referenced(*id), size)
        }
        ExpressionType::Pointer(_, _) => (TypeLayoutField::Pointer, POINTER_SIZE),
        ExpressionType::Collection(CollectionType::Array(_)) => {
            (TypeLayoutField::Pointer, POINTER_SIZE)
        }
        ExpressionType::Collection(CollectionType::Dict(_, _)) => {
            todo!()
        }
        ExpressionType::Nullable(inner) => {
            let (inner, size) = layout_type(declarations, layouts, inner);
            (TypeLayoutField::Nullable(Box::new(inner)), size + 1)
        }
        ExpressionType::ReferenceTo(_) => todo!(),
    }
}
