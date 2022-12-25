use std::{collections::HashMap, iter::Peekable};

use thiserror::Error;

use crate::{
    provenance::Provenance,
    tokenizer::{LexError, Token, TokenValue},
    tree::{Node, NodePtr, SourceTree},
};

#[derive(Debug, PartialEq, Eq)]
pub struct AstStatement {
    pub value: AstStatementValue,
    pub start: Provenance,
    pub end: Provenance,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AstStatementValue {
    FunctionDeclaration {
        name: String,
        params: Vec<NameAndType>,
        returns: Option<usize>,
        body: usize,
    },
    StructDeclaration {
        name: String,
        fields: Vec<NameAndType>,
    },
    Declaration(String, usize),
    Expression(usize),
    Import(String),
}

#[derive(Debug, PartialEq, Eq)]
pub struct NameAndType {
    pub name: String,
    pub kind: usize,
}

#[derive(Debug, PartialEq)]
pub struct AstExpression {
    pub value: AstExpressionValue,
    pub start: Provenance,
    pub end: Provenance,
}

#[derive(Debug, PartialEq)]
pub enum AstExpressionValue {
    Assignment(usize, usize),
    Name(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    BinExpr(BinOp, usize, usize),
    If(usize, usize),
    While(usize, usize),
    Call(usize, Vec<usize>),
    TakeUnique(usize),
    TakeShared(usize),
    StructLiteral {
        name: String,
        fields: HashMap<String, usize>,
    },
    ArrayLiteral(Vec<usize>),
    ArrayLiteralLength(usize, u64),
    /// Importantly, Block references statements, not expressions!
    Block(Vec<usize>),
}

#[derive(Debug, Hash, PartialEq, Eq)]
pub struct AstType {
    pub value: AstTypeValue,
    pub start: Provenance,
    pub end: Provenance,
}

#[derive(Debug, Hash, PartialEq, Eq)]
pub enum AstTypeValue {
    Name(String),
    Unique(usize),
    Shared(usize),
    Array(usize),
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BinOp {
    Dot,
    Add,
    Subtract,
    LessThan,
    GreaterThan,
    Index,
}

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("unexpected token {0}, {1}")]
    UnexpectedLexeme(Box<Token>, &'static str),
    #[error("unexpected end of input at {0}, {1}")]
    UnexpectedEndOfInput(Provenance, &'static str),
    #[error("expected type for parameter at {0}")]
    MissingTypeForParam(Provenance),
    #[error("token error: {0}")]
    TokenError(#[from] LexError),
}

type TokenIterInner<'a> = &'a mut dyn Iterator<Item = Result<Token, LexError>>;
type TokenIter<'a> = Peekable<TokenIterInner<'a>>;

pub type ParseTree = SourceTree<AstStatement, AstExpression, AstType>;

pub fn parse(
    mut source: impl Iterator<Item = Result<Token, LexError>>,
) -> Result<(Vec<usize>, ParseTree), ParseError> {
    let mut source = (&mut source as TokenIterInner<'_>).peekable();
    let mut context = ParseTree::new(Box::new(traverse));

    let mut statements = Vec::new();

    while let Some(lexeme) = peek_token_optional(&mut source)? {
        let start = lexeme.start;
        let statement = statement(&mut source, &mut context, start)?;
        let statement = context.add_statement(statement);
        statements.push(statement);
    }

    Ok((statements, context))
}

fn statement(
    source: &mut TokenIter,
    context: &mut ParseTree,
    start: Provenance,
) -> Result<AstStatement, ParseError> {
    let statement = match peek_token(source, start, "expected let, fn, or expression")? {
        Token {
            value:
                value @ (TokenValue::Let
                | TokenValue::Import
                | TokenValue::Function
                | TokenValue::Struct),
            start,
            ..
        } => {
            let start = *start;
            let value = value.clone();
            source.next();
            match value {
                TokenValue::Let => variable_declaration(source, context, start)?,
                TokenValue::Import => import_declaration(source, start)?,
                TokenValue::Function => function_declaration(source, context, start)?,
                TokenValue::Struct => struct_declaration(source, context, start)?,
                _ => unreachable!(),
            }
        }
        _ => {
            let expr = expression(source, context, start)?;
            let start = expr.start;
            let end = expr.end;
            AstStatement {
                value: AstStatementValue::Expression(context.add_expression(expr)),
                start,
                end,
            }
        }
    };
    if let Some(Token {
        value: TokenValue::Semicolon,
        ..
    }) = peek_token_optional(source)?
    {
        source.next();
    }
    Ok(statement)
}

fn struct_declaration(
    source: &mut TokenIter,
    context: &mut ParseTree,
    start: Provenance,
) -> Result<AstStatement, ParseError> {
    let (name, _, end) = word(source, start, "expected name after 'struct'")?;
    assert_next_lexeme_eq(
        source.next(),
        TokenValue::OpenBracket,
        end,
        "expected open parenthesis to start parameters",
    )?;

    let mut fields = Vec::new();
    let mut pos = end;
    let mut closed = peek_for_closed(
        source,
        TokenValue::CloseBracket,
        pos,
        "expected either fields or close bracket",
    )?;

    while !closed {
        let (name, end, type_hint) =
            name_and_type_hint(source, context, pos, "expected parameter")?;
        let kind = type_hint.ok_or(ParseError::MissingTypeForParam(end))?;
        let kind = context.add_kind(kind);
        pos = end;
        fields.push(NameAndType { name, kind });

        let (should_end, end) = comma_or_end_list(
            source,
            TokenValue::CloseBracket,
            pos,
            "expected either fields or close bracket",
        )?;
        closed = should_end;
        pos = end;
    }

    Ok(AstStatement {
        value: AstStatementValue::StructDeclaration { name, fields },
        start,
        end,
    })
}

fn function_declaration(
    source: &mut TokenIter,
    context: &mut ParseTree,
    start: Provenance,
) -> Result<AstStatement, ParseError> {
    let (name, _, end) = word(source, start, "expected name after 'fn'")?;

    let mut pos = assert_next_lexeme_eq(
        source.next(),
        TokenValue::OpenParen,
        end,
        "expected open parenthesis to start parameters",
    )?
    .end;
    let mut params = Vec::new();
    loop {
        let token = peek_token(source, pos, "expected either parameters or close paren")?;
        pos = token.start;
        match token.value {
            TokenValue::CloseParen => break,
            TokenValue::Comma => {
                source.next();
            }
            _ => {
                let (name, end, type_hint) =
                    name_and_type_hint(source, context, pos, "expected parameter")?;
                let kind = type_hint.ok_or(ParseError::MissingTypeForParam(end))?;
                let kind = context.add_kind(kind);
                pos = end;
                params.push(NameAndType { name, kind });
            }
        }
    }
    assert_next_lexeme_eq(
        source.next(),
        TokenValue::CloseParen,
        pos,
        "expected closing parenthesis to end parameters",
    )?;
    let returns = if let Token {
        value: TokenValue::Colon,
        start,
        ..
    } = peek_token(source, start, "expected body after function declaration")?
    {
        let start = *start;
        source.next();
        let kind = type_expression(source, context, start)?;
        let kind = context.add_kind(kind);
        Some(kind)
    } else {
        None
    };
    let body = block(source, context, start)?;
    let end = body.end;

    Ok(AstStatement {
        value: AstStatementValue::FunctionDeclaration {
            name,
            params,
            returns,
            body: context.add_expression(body),
        },
        start,
        end,
    })
}

fn import_declaration(
    source: &mut TokenIter,
    start: Provenance,
) -> Result<AstStatement, ParseError> {
    let (name, _, end) = word(source, start, "expected word after 'import'")?;

    Ok(AstStatement {
        value: AstStatementValue::Import(name),
        start,
        end,
    })
}

fn variable_declaration(
    source: &mut TokenIter,
    context: &mut ParseTree,
    start: Provenance,
) -> Result<AstStatement, ParseError> {
    // TODO: store type hint in declarations
    let (name, start, _) = name_and_type_hint(
        source,
        context,
        start,
        "expected word after 'let' in declaration",
    )?;
    assert_next_lexeme_eq(
        source.next(),
        TokenValue::Equals,
        start,
        "expected = after let binding target",
    )?;
    let value = expression(source, context, start)?;
    let end = value.end;

    Ok(AstStatement {
        value: AstStatementValue::Declaration(name, context.add_expression(value)),
        start,
        end,
    })
}

fn name_and_type_hint(
    source: &mut TokenIter,
    context: &mut ParseTree,
    start: Provenance,
    reason: &'static str,
) -> Result<(String, Provenance, Option<AstType>), ParseError> {
    let (name, _, mut end) = word(source, start, reason)?;
    let type_hint = if let Some(Ok(Token {
        value: TokenValue::Colon,
        ..
    })) = source.peek()
    {
        source.next();
        let type_hint = type_expression(source, context, start)?;
        end = type_hint.end;
        Some(type_hint)
    } else {
        None
    };

    Ok((name, end, type_hint))
}

fn type_expression(
    source: &mut TokenIter,
    context: &mut ParseTree,
    start: Provenance,
) -> Result<AstType, ParseError> {
    let next = token(source, start, "expected type")?;
    match next.value {
        ptr @ (TokenValue::Unique | TokenValue::Shared) => {
            let subtype = type_expression(source, context, next.end)?;
            let end = subtype.end;
            let subtype = context.add_kind(subtype);
            Ok(AstType {
                value: match ptr {
                    TokenValue::Unique => AstTypeValue::Unique(subtype),
                    TokenValue::Shared => AstTypeValue::Shared(subtype),
                    _ => unreachable!(),
                },
                start: next.start,
                end,
            })
        }
        TokenValue::OpenSquare => {
            let subtype = type_expression(source, context, next.end)?;
            let end = subtype.end;
            assert_next_lexeme_eq(
                source.next(),
                TokenValue::CloseSquare,
                end,
                "expected ] after array type",
            )?;
            let subtype = context.add_kind(subtype);
            Ok(AstType {
                value: AstTypeValue::Array(subtype),
                start: next.start,
                end,
            })
        }
        TokenValue::Word(name) => Ok(AstType {
            value: AstTypeValue::Name(name),
            start: next.start,
            end: next.end,
        }),
        _ => Err(ParseError::UnexpectedLexeme(
            Box::new(next),
            "expected either 'unique' or type name",
        )),
    }
}

fn expression(
    source: &mut TokenIter,
    context: &mut ParseTree,
    provenance: Provenance,
) -> Result<AstExpression, ParseError> {
    match peek_token(source, provenance, "expected expression")? {
        Token {
            value: token @ (TokenValue::If | TokenValue::While),
            start,
            ..
        } => {
            let start = *start;
            let token = token.clone();
            source.next();
            branch(source, context, token, start)
        }
        Token {
            value: TokenValue::OpenBracket,
            start,
            ..
        } => {
            let start = *start;
            block(source, context, start)
        }
        _ => assignment_step(source, context, provenance),
    }
}

fn branch(
    source: &mut TokenIter,
    context: &mut ParseTree,
    token: TokenValue,
    start: Provenance,
) -> Result<AstExpression, ParseError> {
    let predicate = expression(source, context, start)?;
    let block = block(source, context, start)?;

    let predicate_ptr = context.add_expression(predicate);
    let end = block.end;
    let block_ptr = context.add_expression(block);

    Ok(AstExpression {
        value: if token == TokenValue::If {
            AstExpressionValue::If(predicate_ptr, block_ptr)
        } else {
            AstExpressionValue::While(predicate_ptr, block_ptr)
        },
        start,
        end,
    })
}

fn block(
    source: &mut TokenIter,
    context: &mut ParseTree,
    start: Provenance,
) -> Result<AstExpression, ParseError> {
    let start = assert_next_lexeme_eq(
        source.next(),
        TokenValue::OpenBracket,
        start,
        "expected { to start a block",
    )?
    .start;
    let mut statements = Vec::new();
    let mut end = start;
    loop {
        match peek_token(source, start, "expected statement or close bracket")? {
            Token {
                value: TokenValue::CloseBracket,
                end,
                ..
            } => {
                let end = *end;
                source.next();
                return Ok(AstExpression {
                    value: AstExpressionValue::Block(statements),
                    start,
                    end,
                });
            }
            _ => {
                let statement = statement(source, context, end)?;
                end = statement.end;
                statements.push(context.add_statement(statement));
            }
        }
    }
}

fn assignment_step(
    source: &mut TokenIter,
    context: &mut ParseTree,
    provenance: Provenance,
) -> Result<AstExpression, ParseError> {
    let mut root = reference_step(source, context, provenance)?;

    while let Some(Token {
        value: TokenValue::Equals,
        start,
        ..
    }) = peek_token_optional(source)?
    {
        let start = *start;
        source.next(); // remove the peeked token
        let right = assignment_step(source, context, start)?;

        let Span {
            left,
            right,
            start,
            end,
        } = span(context, root, right);

        root = AstExpression {
            value: AstExpressionValue::Assignment(left, right),
            start,
            end,
        };
    }

    Ok(root)
}

fn reference_step(
    source: &mut TokenIter,
    context: &mut ParseTree,
    provenance: Provenance,
) -> Result<AstExpression, ParseError> {
    let next = peek_token(source, provenance, "expected reference or expression")?;
    let start = next.start;
    match &next.value {
        lex @ (TokenValue::Shared | TokenValue::Unique) => {
            let lex = lex.clone();
            source.next();
            let child = reference_step(source, context, start)?;
            let end = child.end;
            let child = context.add_expression(child);
            Ok(AstExpression {
                value: match lex {
                    TokenValue::Shared => AstExpressionValue::TakeShared(child),
                    TokenValue::Unique => AstExpressionValue::TakeUnique(child),
                    _ => unreachable!(),
                },
                start,
                end,
            })
        }
        _ => comparison_step(source, context, provenance),
    }
}

fn comparison_step(
    source: &mut TokenIter,
    context: &mut ParseTree,
    provenance: Provenance,
) -> Result<AstExpression, ParseError> {
    let mut left = sum_step(source, context, provenance)?;

    while let Some(Token {
        value: token @ (TokenValue::LessThan | TokenValue::GreaterThan),
        ..
    }) = peek_token_optional(source)?
    {
        let operator = match token {
            TokenValue::LessThan => BinOp::LessThan,
            TokenValue::GreaterThan => BinOp::GreaterThan,
            _ => unreachable!(),
        };
        source.next();
        let right = comparison_step(source, context, provenance)?;
        left = make_bin_expr(context, operator, left, right);
    }

    Ok(left)
}

fn sum_step(
    source: &mut TokenIter,
    context: &mut ParseTree,
    provenance: Provenance,
) -> Result<AstExpression, ParseError> {
    let mut left = call_step(source, context, provenance)?;

    while let Some(Token {
        value: token @ (TokenValue::Plus | TokenValue::Minus),
        ..
    }) = peek_token_optional(source)?
    {
        let operator = match token {
            TokenValue::Plus => BinOp::Add,
            TokenValue::Minus => BinOp::Subtract,
            _ => unimplemented!(),
        };
        source.next();
        let right = call_step(source, context, provenance)?;
        left = make_bin_expr(context, operator, left, right);
    }

    Ok(left)
}

fn make_bin_expr(
    context: &mut ParseTree,
    operator: BinOp,
    left: AstExpression,
    right: AstExpression,
) -> AstExpression {
    let start = left.start;
    let end = right.end;

    AstExpression {
        value: AstExpressionValue::BinExpr(
            operator,
            context.add_expression(left),
            context.add_expression(right),
        ),
        start,
        end,
    }
}

struct Span {
    left: usize,
    right: usize,
    start: Provenance,
    end: Provenance,
}

fn span(context: &mut ParseTree, left: AstExpression, right: AstExpression) -> Span {
    let start = left.start;
    let end = right.end;
    let left = context.add_expression(left);
    let right = context.add_expression(right);

    Span {
        left,
        right,
        start,
        end,
    }
}

fn call_step(
    source: &mut TokenIter,
    context: &mut ParseTree,
    provenance: Provenance,
) -> Result<AstExpression, ParseError> {
    let mut root = dot_step(source, context, provenance)?;

    while let Some(Token {
        value: opening @ (TokenValue::OpenParen | TokenValue::OpenSquare),
        end,
        ..
    }) = peek_token_optional(source)?
    {
        let is_square = opening == &TokenValue::OpenSquare;
        let mut end = *end;
        source.next(); // remove the peeked token
        if is_square {
            let index = expression(source, context, end)?;
            assert_next_lexeme_eq(
                source.next(),
                TokenValue::CloseSquare,
                end,
                "expected ] to follow array index",
            )?;
            let index = context.add_expression(index);
            let start = root.start;
            let left = context.add_expression(root);
            root = AstExpression {
                value: AstExpressionValue::BinExpr(BinOp::Index, left, index),
                start,
                end,
            };
        } else {
            let mut arguments = Vec::new();

            let mut closed = peek_for_closed(
                source,
                TokenValue::CloseParen,
                end,
                "expected ) or next argument",
            )?;

            while !closed {
                let argument = expression(source, context, provenance)?;
                end = argument.end;
                arguments.push(context.add_expression(argument));

                let (should_break, new_end) = comma_or_end_list(
                    source,
                    TokenValue::CloseParen,
                    end,
                    "expected comma or ) to end function call",
                )?;
                end = new_end;
                closed = should_break;
            }

            let start = root.start;
            let function = context.add_expression(root);

            root = AstExpression {
                value: AstExpressionValue::Call(function, arguments),
                start,
                end,
            }
        }
    }

    Ok(root)
}

fn dot_step(
    source: &mut TokenIter,
    context: &mut ParseTree,
    provenance: Provenance,
) -> Result<AstExpression, ParseError> {
    let mut root = paren_step(source, context, provenance)?;
    while let Some(Token {
        value: TokenValue::Period,
        ..
    }) = peek_token_optional(source)?
    {
        source.next();
        let (name, start, end) = word(source, provenance, "expected a name after dot operator")?;
        let right = context.add_expression(AstExpression {
            value: AstExpressionValue::Name(name),
            start,
            end,
        });
        let start = root.start;
        let left = context.add_expression(root);
        root = AstExpression {
            value: AstExpressionValue::BinExpr(BinOp::Dot, left, right),
            start,
            end,
        };
    }

    Ok(root)
}

fn paren_step(
    source: &mut TokenIter,
    context: &mut ParseTree,
    provenance: Provenance,
) -> Result<AstExpression, ParseError> {
    let peeked = peek_token(source, provenance, "expected (, [, or expression")?;
    match peeked.value {
        TokenValue::OpenParen => {
            let start = peeked.start;
            source.next();
            paren(source, context, start)
        }
        TokenValue::OpenSquare => {
            let start = peeked.start;
            source.next();
            array_literal(source, context, start)
        }
        _ => struct_literal_step(source, context, provenance),
    }
}

fn paren(
    source: &mut TokenIter,
    context: &mut ParseTree,
    provenance: Provenance,
) -> Result<AstExpression, ParseError> {
    let expr = expression(source, context, provenance)?;
    assert_next_lexeme_eq(
        source.next(),
        TokenValue::CloseParen,
        expr.start,
        "parenthesized expressions must end with a )",
    )?;

    Ok(expr)
}

fn array_literal(
    source: &mut TokenIter,
    context: &mut ParseTree,
    start: Provenance,
) -> Result<AstExpression, ParseError> {
    let expr = expression(source, context, start)?;
    let separator = token(source, start, "expected comma, semicolon or ]")?;
    match separator.value {
        TokenValue::Comma => {
            let mut end = separator.end;
            let mut children = vec![context.add_expression(expr)];

            let mut closed = peek_for_closed(
                source,
                TokenValue::CloseSquare,
                end,
                "expected ] or next argument",
            )?;

            while !closed {
                let peeked = peek_token(source, end, "expected , or ]")?;
                if TokenValue::CloseSquare == peeked.value {
                    source.next();
                    break;
                }
                end = peeked.end;
                let expr = expression(source, context, end)?;
                end = expr.end;
                children.push(context.add_expression(expr));

                let (should_break, new_end) = comma_or_end_list(
                    source,
                    TokenValue::CloseSquare,
                    end,
                    "expected comma or ] to end array literal",
                )?;
                end = new_end;
                closed = should_break;
            }
            Ok(AstExpression {
                value: AstExpressionValue::ArrayLiteral(children),
                start,
                end,
            })
        }
        TokenValue::Semicolon => {
            let (length, _, end) =
                integer(source, start, "expected number after ; in array literal")?;
            let close = assert_next_lexeme_eq(
                source.next(),
                TokenValue::CloseSquare,
                end,
                "expected ] after array length in array literal",
            )?;
            let expr = context.add_expression(expr);
            Ok(AstExpression {
                value: AstExpressionValue::ArrayLiteralLength(expr, length),
                start,
                end: close.end,
            })
        }
        TokenValue::CloseSquare => {
            let expr = context.add_expression(expr);
            Ok(AstExpression {
                value: AstExpressionValue::ArrayLiteral(vec![expr]),
                start,
                end: separator.end,
            })
        }
        _ => Err(ParseError::UnexpectedLexeme(
            Box::new(separator),
            "expected comma, semicolon or ]",
        )),
    }
}

fn struct_literal_step(
    source: &mut TokenIter,
    context: &mut ParseTree,
    provenance: Provenance,
) -> Result<AstExpression, ParseError> {
    let atom = atom(source, provenance, "expected a word or atom")?;
    match (atom, peek_token_optional(source)?) {
        (
            AstExpression {
                value: AstExpressionValue::Name(name),
                start,
                ..
            },
            Some(Token {
                value: TokenValue::OpenBracket,
                end,
                ..
            }),
        ) => {
            let name = name;
            let start = start;
            let mut end = *end;
            source.next();
            let mut fields = HashMap::new();

            loop {
                if TokenValue::CloseBracket
                    == peek_token(source, end, "expected } or next field")?.value
                {
                    source.next();
                    break;
                }

                let (field, field_start, field_end) =
                    word(source, end, "expected field in struct literal")?;
                if let lex @ (TokenValue::Comma | TokenValue::CloseBracket) =
                    &peek_token(source, end, "expected comma or } to end struct literal")?.value
                {
                    let lex = lex.clone();
                    source.next();
                    let argument = AstExpression {
                        value: AstExpressionValue::Name(field.clone()),
                        start: field_start,
                        end: field_end,
                    };
                    fields.insert(field, context.add_expression(argument));
                    if lex == TokenValue::CloseBracket {
                        break;
                    }
                } else {
                    assert_next_lexeme_eq(
                        source.next(),
                        TokenValue::Colon,
                        end,
                        "expected colon after field name",
                    )?;
                    let argument = expression(source, context, provenance)?;
                    end = argument.end;
                    fields.insert(field, context.add_expression(argument));

                    if let TokenValue::Comma =
                        peek_token(source, end, "expected comma or ) to end function call")?.value
                    {
                        source.next();
                    } else {
                        assert_next_lexeme_eq(
                            source.next(),
                            TokenValue::CloseBracket,
                            end,
                            "expected close parenthesis or comma after argument",
                        )?;
                        break;
                    }
                }
            }
            Ok(AstExpression {
                value: AstExpressionValue::StructLiteral { name, fields },
                start,
                end,
            })
        }
        (atom, _) => Ok(atom),
    }
}

fn atom(
    source: &mut TokenIter,
    provenance: Provenance,
    reason: &'static str,
) -> Result<AstExpression, ParseError> {
    // TODO: expectation reasoning
    let Token { value, start, end } = token(source, provenance, reason)?;
    match value {
        TokenValue::True => Ok(AstExpression {
            value: AstExpressionValue::Bool(true),
            start,
            end,
        }),
        TokenValue::False => Ok(AstExpression {
            value: AstExpressionValue::Bool(false),
            start,
            end,
        }),
        TokenValue::Word(word) => Ok(AstExpression {
            value: AstExpressionValue::Name(word),
            start,
            end,
        }),
        TokenValue::Int(int) => try_decimal(source, int as i64, start, end),
        // TODO: should this be treated as a unary operator instead?
        TokenValue::Minus => {
            let (int, _, end) = integer(source, start, "expected digit after negative sign")?;
            try_decimal(source, -(int as i64), start, end)
        }
        value => Err(ParseError::UnexpectedLexeme(
            Box::new(Token { value, start, end }),
            reason,
        )),
    }
}

fn integer(
    source: &mut TokenIter,
    start: Provenance,
    reason: &'static str,
) -> Result<(u64, Provenance, Provenance), ParseError> {
    match token(source, start, reason)? {
        Token {
            value: TokenValue::Int(int),
            start,
            end,
        } => Ok((int, start, end)),
        other => Err(ParseError::UnexpectedLexeme(Box::new(other), reason)),
    }
}

fn try_decimal(
    source: &mut TokenIter,
    num: i64,
    start: Provenance,
    end: Provenance,
) -> Result<AstExpression, ParseError> {
    // TODO: handle overflow
    if let Some(Token {
        value: TokenValue::Period,
        ..
    }) = peek_token_optional(source)?
    {
        source.next();
        let (decimal, end) = match source.next().ok_or(ParseError::UnexpectedEndOfInput(
            start,
            "expected digit after decimal point",
        ))?? {
            Token {
                value: TokenValue::Int(value),
                end,
                ..
            } => (value as f64, end),
            other => {
                return Err(ParseError::UnexpectedLexeme(
                    Box::new(other),
                    "expected number after decimal point",
                ))
            }
        };
        let num = num as f64;
        let num = if decimal != 0.0 {
            num + decimal.copysign(num) * (10f64).powf(-decimal.log(10.0).ceil())
        } else {
            num
        };
        Ok(AstExpression {
            value: AstExpressionValue::Float(num),
            start,
            end,
        })
    } else {
        Ok(AstExpression {
            value: AstExpressionValue::Int(num),
            start,
            end,
        })
    }
}

fn token(
    source: &mut TokenIter,
    provenance: Provenance,
    reason: &'static str,
) -> Result<Token, ParseError> {
    let lex = source
        .next()
        .ok_or(ParseError::UnexpectedEndOfInput(provenance, reason))??;
    Ok(lex)
}

fn word(
    source: &mut TokenIter,
    provenance: Provenance,
    reason: &'static str,
) -> Result<(String, Provenance, Provenance), ParseError> {
    match token(source, provenance, reason)? {
        Token {
            value: TokenValue::Word(name),
            start,
            end,
            ..
        } => Ok((name, start, end)),
        other => Err(ParseError::UnexpectedLexeme(Box::new(other), reason)),
    }
}

fn peek_token<'a>(
    source: &'a mut TokenIter,
    provenance: Provenance,
    reason: &'static str,
) -> Result<&'a Token, ParseError> {
    Ok(source
        .peek()
        .ok_or(ParseError::UnexpectedEndOfInput(provenance, reason))?
        .as_ref()
        .map_err(|e| e.clone())?)
}

fn peek_token_optional<'a>(source: &'a mut TokenIter) -> Result<Option<&'a Token>, ParseError> {
    Ok(source
        .peek()
        .map(|result| result.as_ref().map_err(|e| e.clone()))
        .transpose()?)
}

fn traverse(root: Node<&AstStatement, &AstExpression, &AstType>, children: &mut Vec<NodePtr>) {
    use AstExpressionValue::*;
    use AstStatementValue::*;
    use AstTypeValue::*;

    match root {
        Node::Statement(AstStatement {
            value:
                Declaration(_, child) | Expression(child) | FunctionDeclaration { body: child, .. },
            ..
        })
        | Node::Expression(AstExpression {
            value: TakeShared(child) | TakeUnique(child) | ArrayLiteralLength(child, _),
            ..
        }) => {
            children.push(NodePtr::Expression(*child));
        }
        Node::Expression(AstExpression {
            value:
                Assignment(left, right) | BinExpr(_, left, right) | If(left, right) | While(left, right),
            ..
        }) => {
            children.push(NodePtr::Expression(*right));
            children.push(NodePtr::Expression(*left));
        }
        Node::Expression(AstExpression {
            value: ArrayLiteral(values),
            ..
        }) => {
            for value in values {
                children.push(NodePtr::Expression(*value));
            }
        }
        Node::Expression(AstExpression {
            value: Call(function, parameters),
            ..
        }) => {
            children.push(NodePtr::Expression(*function));
            for expression in parameters {
                children.push(NodePtr::Expression(*expression));
            }
        }
        Node::Expression(AstExpression {
            value: Block(statements),
            ..
        }) => {
            for statement in statements {
                children.push(NodePtr::Statement(*statement));
            }
        }
        Node::Expression(AstExpression {
            value: StructLiteral { fields, .. },
            ..
        }) => {
            for expression in fields.values() {
                children.push(NodePtr::Expression(*expression));
            }
        }
        Node::Kind(AstType {
            value: Unique(kind) | Shared(kind) | Array(kind),
            ..
        }) => {
            children.push(NodePtr::Kind(*kind));
        }
        Node::Kind(AstType {
            value: AstTypeValue::Name(_),
            ..
        })
        | Node::Statement(AstStatement {
            value: Import(_) | StructDeclaration { .. },
            ..
        })
        | Node::Expression(AstExpression {
            value: AstExpressionValue::Name(_) | Int(_) | Float(_) | Bool(_),
            ..
        }) => {}
    }
}

fn assert_next_lexeme_eq(
    lexeme: Option<Result<Token, LexError>>,
    target: TokenValue,
    provenance: Provenance,
    reason: &'static str,
) -> Result<Token, ParseError> {
    let lexeme = lexeme.ok_or(ParseError::UnexpectedEndOfInput(provenance, reason))??;
    assert_lexeme_eq(&lexeme, target, reason)?;

    Ok(lexeme)
}

fn assert_lexeme_eq(
    lexeme: &Token,
    target: TokenValue,
    reason: &'static str,
) -> Result<(), ParseError> {
    if lexeme.value == target {
        Ok(())
    } else {
        Err(ParseError::UnexpectedLexeme(
            Box::new(lexeme.clone()),
            reason,
        ))
    }
}

/**
 * Returns (did_list_end, end_of_token)
 */
fn comma_or_end_list(
    source: &mut TokenIter,
    list_end: TokenValue,
    start: Provenance,
    reason: &'static str,
) -> Result<(bool, Provenance), ParseError> {
    let next = token(source, start, reason)?;
    match next.value {
        TokenValue::Comma => match peek_token_optional(source)? {
            Some(token) if token.value == list_end => {
                let end = token.end;
                source.next();
                Ok((true, end))
            }
            _ => Ok((false, next.end)),
        },
        other if other == list_end => Ok((true, next.end)),
        _ => Err(ParseError::UnexpectedLexeme(Box::new(next), reason)),
    }
}

fn peek_for_closed(
    source: &mut TokenIter,
    list_end: TokenValue,
    provenance: Provenance,
    reason: &'static str,
) -> Result<bool, ParseError> {
    let closed = peek_token(source, provenance, reason)?.value == list_end;
    if closed {
        source.next();
    }

    Ok(closed)
}

#[cfg(test)]
mod test {
    use matches::assert_matches;

    use super::*;

    fn tokens(tokens: &[TokenValue]) -> impl '_ + Iterator<Item = Result<Token, LexError>> {
        let provenance = Provenance::new("test", "test", 0, 0);
        tokens.iter().map(move |token| {
            Ok(Token {
                value: token.clone(),
                start: provenance,
                end: provenance,
            })
        })
    }

    #[test]
    fn adding() {
        let (statements, ast) = parse(tokens(&[
            TokenValue::Let,
            TokenValue::Word("a".to_string()),
            TokenValue::Equals,
            TokenValue::OpenParen,
            TokenValue::Int(15),
            TokenValue::Minus,
            TokenValue::Int(10),
            TokenValue::CloseParen,
            TokenValue::Plus,
            TokenValue::Int(3),
            TokenValue::Semicolon,
            TokenValue::If,
            TokenValue::True,
            TokenValue::OpenBracket,
            TokenValue::Word("a".to_string()),
            TokenValue::Equals,
            TokenValue::Int(3),
            TokenValue::Semicolon,
            TokenValue::CloseBracket,
            TokenValue::Word("a".to_string()),
        ]))
        .unwrap();
        let nodes = statements
            .into_iter()
            .map(|statement| {
                let mut line = Vec::new();
                line.extend(ast.iter_from(NodePtr::Statement(statement)));
                line
            })
            .collect::<Vec<_>>();
        let matchable_nodes = nodes.iter().map(|line| &line[..]).collect::<Vec<_>>();

        use AstExpressionValue::*;
        use AstStatementValue::*;
        use Node::{Expression as Expr, Statement};
        assert_matches!(
            matchable_nodes.as_slice(),
            &[
                &[
                    Statement(AstStatement {
                        value: Declaration(_, _),
                        ..
                    }),
                    Expr(AstExpression {
                        value: BinExpr(BinOp::Add, _, _),
                        ..
                    }),
                    Expr(AstExpression {
                        value: BinExpr(BinOp::Subtract, _, _),
                        ..
                    }),
                    Expr(AstExpression { value: Int(15), .. }),
                    Expr(AstExpression { value: Int(10), .. }),
                    Expr(AstExpression { value: Int(3), .. }),
                ],
                &[
                    Statement(AstStatement {
                        value: Expression(_),
                        ..
                    }),
                    Expr(AstExpression {
                        value: If(_, _),
                        ..
                    }),
                    Expr(AstExpression {
                        value: Bool(true),
                        ..
                    }),
                    Expr(AstExpression {
                        value: Block(_),
                        ..
                    }),
                    Statement(AstStatement {
                        value: Expression(_),
                        ..
                    }),
                    Expr(AstExpression {
                        value: Assignment(_, _),
                        ..
                    }),
                    Expr(AstExpression { value: Name(_), .. }),
                    Expr(AstExpression { value: Int(3), .. }),
                ],
                &[
                    Statement(AstStatement {
                        value: Expression(_),
                        ..
                    }),
                    Expr(AstExpression { value: Name(_), .. }),
                ],
            ]
        );
    }

    #[test]
    fn function() {
        let (statements, ast) = parse(tokens(&[
            TokenValue::Function,
            TokenValue::Word("f".to_string()),
            TokenValue::OpenParen,
            TokenValue::Word("argument".to_string()),
            TokenValue::Colon,
            TokenValue::Word("i64".to_string()),
            TokenValue::CloseParen,
            TokenValue::Colon,
            TokenValue::Word("i64".to_string()),
            TokenValue::OpenBracket,
            TokenValue::Int(3),
            TokenValue::Plus,
            TokenValue::Int(5),
            TokenValue::CloseBracket,
        ]))
        .unwrap();
        let nodes = statements
            .into_iter()
            .map(|statement| {
                let mut line = Vec::new();
                line.extend(ast.iter_from(NodePtr::Statement(statement)));
                line
            })
            .collect::<Vec<_>>();
        let matchable_nodes = nodes.iter().map(|line| &line[..]).collect::<Vec<_>>();

        use AstExpressionValue::*;
        use AstStatementValue::*;
        use Node::{Expression as Expr, Statement};
        assert_matches!(
            matchable_nodes.as_slice(),
            &[&[
                Statement(AstStatement {
                    value: FunctionDeclaration {
                        returns: Some(_),
                        ..
                    },
                    ..
                }),
                Expr(AstExpression {
                    value: Block(_),
                    ..
                }),
                Statement(AstStatement {
                    value: Expression(_),
                    ..
                }),
                Expr(AstExpression {
                    value: BinExpr(BinOp::Add, _, _),
                    ..
                }),
                Expr(AstExpression { value: Int(3), .. }),
                Expr(AstExpression { value: Int(5), .. }),
            ],]
        );
    }
}
