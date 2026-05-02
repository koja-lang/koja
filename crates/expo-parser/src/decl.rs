use expo_ast::ast::*;
use expo_ast::token::TokenKind;

use crate::parser::Parser;

impl Parser {
    // =========================================================================
    // Struct
    // =========================================================================

    pub(crate) fn parse_struct_item(&mut self) -> Item {
        self.parse_struct_item_with_annotations(Vec::new())
    }

    pub(crate) fn parse_struct_item_with_annotations(
        &mut self,
        annotations: Vec<Annotation>,
    ) -> Item {
        let start = self.current_span();
        self.advance(); // struct

        let name = self.expect_type_ident();
        let type_params = self.parse_optional_type_params();

        self.skip_newlines();
        let mut fields = Vec::new();
        let mut functions = Vec::new();
        while !self.at(&TokenKind::End) && !self.at_eof() {
            match self.peek().clone() {
                TokenKind::Fn | TokenKind::Priv | TokenKind::At => {
                    functions.push(self.parse_type_body_function("struct"));
                }
                _ => {
                    fields.push(self.parse_struct_field());
                }
            }
            self.skip_newlines();
        }
        self.expect(&TokenKind::End);

        Item::Struct(StructDecl {
            annotations,
            name,
            type_params,
            fields,
            functions,
            span: self.span_from(start),
        })
    }

    fn parse_struct_field(&mut self) -> StructField {
        let start = self.current_span();
        let name = self.expect_ident();
        self.expect(&TokenKind::Colon);
        let type_expr = self.parse_type_expr();
        let default = if self.eat(&TokenKind::Eq).is_some() {
            Some(self.parse_expr())
        } else {
            None
        };
        StructField {
            name,
            type_expr,
            default,
            span: self.span_from(start),
        }
    }

    // =========================================================================
    // Shared: parse a function inside a struct or enum body
    // =========================================================================

    /// Parses a single function declaration inside a struct or enum body.
    /// Handles `fn`, `priv fn`, and `@annotation fn` / `@annotation priv fn`.
    fn parse_type_body_function(&mut self, context: &str) -> Function {
        match self.peek().clone() {
            TokenKind::Fn => self.parse_function_decl(Vec::new(), Visibility::Public),
            TokenKind::Priv => {
                self.advance();
                self.parse_function_decl(Vec::new(), Visibility::Private)
            }
            TokenKind::At => {
                let annotations = self.parse_annotations();
                self.skip_newlines();
                match self.peek() {
                    TokenKind::Fn => self.parse_function_decl(annotations, Visibility::Public),
                    TokenKind::Priv => {
                        self.advance();
                        self.parse_function_decl(annotations, Visibility::Private)
                    }
                    _ => {
                        let span = self.current_span();
                        self.error(
                            format!("annotation in {context} block must be followed by a function"),
                            span,
                        );
                        self.parse_function_decl(annotations, Visibility::Public)
                    }
                }
            }
            _ => unreachable!("parse_type_body_function called on non-function token"),
        }
    }

    // =========================================================================
    // Enum
    // =========================================================================

    pub(crate) fn parse_enum_item(&mut self) -> Item {
        self.parse_enum_item_with_annotations(Vec::new())
    }

    pub(crate) fn parse_enum_item_with_annotations(
        &mut self,
        annotations: Vec<Annotation>,
    ) -> Item {
        let start = self.current_span();
        self.advance(); // enum

        let name = self.expect_type_ident();
        let type_params = self.parse_optional_type_params();

        self.skip_newlines();
        let mut variants = Vec::new();
        let mut functions = Vec::new();
        while !self.at(&TokenKind::End) && !self.at_eof() {
            match self.peek().clone() {
                TokenKind::Fn | TokenKind::Priv | TokenKind::At => {
                    functions.push(self.parse_type_body_function("enum"));
                }
                _ => {
                    variants.push(self.parse_enum_variant());
                }
            }
            self.skip_newlines();
        }
        self.expect(&TokenKind::End);

        Item::Enum(EnumDecl {
            annotations,
            name,
            type_params,
            variants,
            functions,
            span: self.span_from(start),
        })
    }

    fn parse_enum_variant(&mut self) -> EnumVariant {
        let start = self.current_span();
        let name = self.expect_type_ident();

        let data = if self.eat(&TokenKind::LParen).is_some() {
            self.skip_newlines();
            let mut types = vec![self.parse_type_expr()];
            while self.eat(&TokenKind::Comma).is_some() {
                self.skip_newlines();
                if self.at(&TokenKind::RParen) {
                    break;
                }
                types.push(self.parse_type_expr());
            }
            self.skip_newlines();
            self.expect(&TokenKind::RParen);
            EnumVariantData::Tuple(types)
        } else if self.eat(&TokenKind::LBrace).is_some() {
            let mut fields = Vec::new();
            self.skip_newlines();
            while !self.at(&TokenKind::RBrace) && !self.at_eof() {
                fields.push(self.parse_struct_field());
                if self.eat(&TokenKind::Comma).is_none() {
                    self.skip_newlines();
                    if !self.at(&TokenKind::RBrace) {
                        self.skip_newlines();
                    }
                } else {
                    self.skip_newlines();
                }
            }
            self.expect(&TokenKind::RBrace);
            EnumVariantData::Struct(fields)
        } else {
            EnumVariantData::Unit
        };

        EnumVariant {
            name,
            data,
            span: self.span_from(start),
        }
    }

    // =========================================================================
    // Protocol
    // =========================================================================

    pub(crate) fn parse_protocol_item(&mut self, annotations: Vec<Annotation>) -> Item {
        let start = self.current_span();
        self.advance(); // protocol

        let name = self.expect_type_ident();
        let type_params = self.parse_optional_type_params();

        self.skip_newlines();
        let mut methods = Vec::new();
        while !self.at(&TokenKind::End) && !self.at_eof() {
            self.skip_newlines();
            if self.at(&TokenKind::End) {
                break;
            }
            match self.peek().clone() {
                TokenKind::Fn => {
                    methods.push(self.parse_protocol_method(Vec::new()));
                }
                TokenKind::At => {
                    let anns = self.parse_annotations();
                    self.skip_newlines();
                    if self.at(&TokenKind::Fn) {
                        methods.push(self.parse_protocol_method(anns));
                    } else {
                        let span = self.current_span();
                        self.error(
                            "annotation in protocol must be followed by a function signature"
                                .to_string(),
                            span,
                        );
                    }
                }
                _ => {
                    let span = self.current_span();
                    self.error(
                        format!(
                            "expected function signature in protocol, found {:?}",
                            self.peek()
                        ),
                        span,
                    );
                    self.advance();
                }
            }
            self.skip_newlines();
        }
        self.expect(&TokenKind::End);

        Item::Protocol(ProtocolDecl {
            annotations,
            name,
            type_params,
            methods,
            span: self.span_from(start),
        })
    }

    fn parse_protocol_method(&mut self, annotations: Vec<Annotation>) -> ProtocolMethod {
        let start = self.current_span();
        self.advance(); // fn

        let name = self.expect_ident();
        let type_params = self.parse_optional_type_params();

        let params = if self.at(&TokenKind::LParen) {
            self.advance();
            let params = if self.at(&TokenKind::RParen) {
                Vec::new()
            } else {
                self.parse_param_list()
            };
            self.expect(&TokenKind::RParen);
            params
        } else {
            Vec::new()
        };

        self.skip_newlines();
        let return_type = if self.eat(&TokenKind::Arrow).is_some() {
            Some(self.parse_type_expr())
        } else {
            None
        };

        self.skip_newlines();
        let body =
            if !self.at(&TokenKind::End) && !self.at(&TokenKind::Fn) && !self.at(&TokenKind::At) {
                let stmts = self.parse_block();
                self.expect(&TokenKind::End);
                Some(stmts)
            } else {
                None
            };

        ProtocolMethod {
            annotations,
            name,
            type_params,
            params,
            return_type,
            body,
            span: self.span_from(start),
        }
    }

    // =========================================================================
    // Impl
    // =========================================================================

    pub(crate) fn parse_impl_item(&mut self) -> Item {
        let start = self.current_span();
        self.advance(); // impl

        let first_type = self.parse_type_expr();

        let (final_target, final_trait) = if self.eat(&TokenKind::For).is_some() {
            let actual_target = self.parse_type_expr();
            (actual_target, Some(first_type))
        } else {
            (first_type, None)
        };

        self.skip_newlines();
        let mut members = Vec::new();
        while !self.at(&TokenKind::End) && !self.at_eof() {
            self.skip_newlines();
            if self.at(&TokenKind::End) {
                break;
            }
            match self.peek().clone() {
                TokenKind::Fn | TokenKind::Priv | TokenKind::At => {
                    let func = self.parse_type_body_function("impl");
                    members.push(ImplMember::Function(func));
                }
                TokenKind::Type => {
                    let alias = self.parse_type_alias(Vec::new());
                    members.push(ImplMember::TypeAlias(alias));
                }
                _ => {
                    let span = self.current_span();
                    self.error(
                        format!(
                            "expected function or type alias in impl block, found {:?}",
                            self.peek()
                        ),
                        span,
                    );
                    self.advance();
                }
            }
            self.skip_newlines();
        }
        self.expect(&TokenKind::End);

        Item::Impl(ImplBlock {
            target: final_target,
            trait_expr: final_trait,
            members,
            span: self.span_from(start),
        })
    }

    fn parse_type_alias(&mut self, annotations: Vec<Annotation>) -> TypeAlias {
        let start = self.current_span();
        self.advance(); // type
        let name = self.expect_type_ident();
        self.expect(&TokenKind::Eq);
        let type_expr = self.parse_type_expr();
        TypeAlias {
            annotations,
            name,
            type_expr,
            span: self.span_from(start),
        }
    }

    pub(crate) fn parse_type_alias_item(&mut self, annotations: Vec<Annotation>) -> Item {
        let alias = self.parse_type_alias(annotations);
        Item::TypeAlias(alias)
    }

    pub(crate) fn parse_alias_item(&mut self) -> Item {
        let start = self.current_span();
        self.advance(); // alias

        let mut path = Vec::new();
        // Parse dotted path: lowercase.segments.TypeName
        loop {
            match self.peek().clone() {
                TokenKind::Ident(name) => {
                    self.advance();
                    path.push(name);
                    if self.eat(&TokenKind::Dot).is_none() {
                        let span = self.current_span();
                        self.error(
                            "alias path must end with a type name (PascalCase)".to_string(),
                            span,
                        );
                        break;
                    }
                }
                TokenKind::TypeIdent(name) => {
                    self.advance();
                    path.push(name);
                    break;
                }
                _ => {
                    let span = self.current_span();
                    self.error(
                        format!("expected package path in alias, found {:?}", self.peek()),
                        span,
                    );
                    break;
                }
            }
        }

        let local_name = if self.eat(&TokenKind::Ident("as".to_string())).is_some() {
            self.expect_type_ident()
        } else {
            path.last().cloned().unwrap_or_default()
        };

        Item::Alias(AliasDecl {
            path,
            local_name,
            span: self.span_from(start),
        })
    }

    // =========================================================================
    // Function
    // =========================================================================

    pub(crate) fn parse_function_item(
        &mut self,
        annotations: Vec<Annotation>,
        visibility: Visibility,
    ) -> Item {
        Item::Function(self.parse_function_decl(annotations, visibility))
    }

    pub(crate) fn parse_function_decl(
        &mut self,
        annotations: Vec<Annotation>,
        visibility: Visibility,
    ) -> Function {
        let start = self.current_span();
        self.advance(); // fn

        let name = self.expect_ident();
        let type_params = self.parse_optional_type_params();

        let params = if self.at(&TokenKind::LParen) {
            self.advance(); // (
            let params = if self.at(&TokenKind::RParen) {
                Vec::new()
            } else {
                self.parse_param_list()
            };
            self.expect(&TokenKind::RParen);
            params
        } else {
            Vec::new()
        };

        self.skip_newlines();
        let return_type = if self.eat(&TokenKind::Arrow).is_some() {
            Some(self.parse_type_expr())
        } else {
            None
        };

        self.skip_newlines();
        let bodyless_marker = annotations
            .iter()
            .any(|a| a.name == "extern" || a.name == "intrinsic");
        let body = if bodyless_marker
            || self.at(&TokenKind::Fn)
            || self.at(&TokenKind::At)
            || (self.at(&TokenKind::Priv) && matches!(self.peek_nth(1), TokenKind::Fn))
        {
            None
        } else if self.at(&TokenKind::End) {
            self.advance();
            Some(Vec::new())
        } else {
            let stmts = self.parse_block();
            self.expect(&TokenKind::End);
            Some(stmts)
        };

        Function {
            annotations,
            visibility,
            name,
            type_params,
            params,
            return_type,
            body,
            span: self.span_from(start),
        }
    }

    fn parse_param_list(&mut self) -> Vec<Param> {
        let mut params = Vec::new();
        self.skip_newlines();
        params.push(self.parse_param());
        while self.eat(&TokenKind::Comma).is_some() {
            self.skip_newlines();
            if self.at(&TokenKind::RParen) {
                break;
            }
            params.push(self.parse_param());
        }
        self.skip_newlines();
        params
    }

    fn parse_param(&mut self) -> Param {
        let start = self.current_span();

        let has_move = self.eat(&TokenKind::Move).is_some();

        if self.eat(&TokenKind::Self_).is_some() {
            return Param::Self_ {
                mode: if has_move {
                    PassMode::Move
                } else {
                    PassMode::Borrow
                },
                span: self.span_from(start),
            };
        }

        let mode = if has_move || self.eat(&TokenKind::Move).is_some() {
            PassMode::Move
        } else {
            PassMode::Borrow
        };
        let name = self.expect_ident();
        self.expect(&TokenKind::Colon);
        let type_expr = self.parse_type_expr();
        let default = if self.eat(&TokenKind::Eq).is_some() {
            Some(self.parse_expr())
        } else {
            None
        };

        Param::Regular {
            mode,
            name,
            type_expr,
            default,
            span: self.span_from(start),
        }
    }

    // =========================================================================
    // Annotation
    // =========================================================================

    pub(crate) fn parse_annotations(&mut self) -> Vec<Annotation> {
        let mut annotations = Vec::new();
        while self.at(&TokenKind::At) {
            annotations.push(self.parse_annotation());
            self.skip_newlines();
        }
        annotations
    }

    pub(crate) fn parse_annotation(&mut self) -> Annotation {
        let start = self.current_span();
        self.advance(); // @
        let name = self.expect_ident();
        let value = self.parse_annotation_value();
        Annotation {
            name,
            value,
            span: self.span_from(start),
        }
    }

    fn parse_annotation_value(&mut self) -> Option<AnnotationValue> {
        match self.peek() {
            TokenKind::False => {
                self.advance();
                Some(AnnotationValue::False)
            }
            TokenKind::StringStart => {
                self.advance(); // StringStart
                let mut text = String::new();
                loop {
                    match self.peek().clone() {
                        TokenKind::StringFragment(s) => {
                            text.push_str(&s);
                            self.advance();
                        }
                        TokenKind::StringEnd => {
                            self.advance();
                            break;
                        }
                        _ => break,
                    }
                }
                Some(AnnotationValue::String(text))
            }
            TokenKind::MultilineStringStart => {
                self.advance();
                let mut text = String::new();
                loop {
                    match self.peek().clone() {
                        TokenKind::StringFragment(s) => {
                            text.push_str(&s);
                            self.advance();
                        }
                        TokenKind::MultilineStringEnd => {
                            self.advance();
                            break;
                        }
                        _ => break,
                    }
                }
                Some(AnnotationValue::String(text))
            }
            _ => None,
        }
    }

    // =========================================================================
    // Shared / Constant
    // =========================================================================

    pub(crate) fn parse_shared_item(&mut self) -> Item {
        let start = self.current_span();
        self.advance(); // shared
        let name = self.expect_ident();
        self.expect(&TokenKind::Colon);
        let type_expr = self.parse_type_expr();
        Item::Shared(SharedDecl {
            name,
            type_expr,
            span: self.span_from(start),
        })
    }

    pub(crate) fn parse_constant_item(&mut self, annotations: Vec<Annotation>) -> Item {
        let start = self.current_span();
        self.expect(&TokenKind::Const);
        let name = match self.peek().clone() {
            TokenKind::Ident(name) | TokenKind::TypeIdent(name) => {
                self.advance();
                name
            }
            _ => {
                let span = self.current_span();
                self.error(
                    format!("expected constant name, found {:?}", self.peek()),
                    span,
                );
                self.advance();
                String::from("<error>")
            }
        };
        let type_annotation = if self.peek() == &TokenKind::Colon {
            self.advance();
            Some(self.parse_type_expr())
        } else {
            None
        };
        self.expect(&TokenKind::Eq);
        let value = self.parse_expr();
        Item::Constant(Constant {
            annotations,
            name,
            type_annotation,
            value,
            span: self.span_from(start),
        })
    }

    // =========================================================================
    // Helpers
    // =========================================================================

    pub(crate) fn parse_optional_type_params(&mut self) -> Vec<TypeParam> {
        if self.eat(&TokenKind::Lt).is_none() {
            return Vec::new();
        }
        let mut params = vec![self.parse_type_param()];
        while self.eat(&TokenKind::Comma).is_some() {
            params.push(self.parse_type_param());
        }
        self.expect_gt();
        params
    }

    fn parse_type_param(&mut self) -> TypeParam {
        let span = self.current_span();
        let name = self.expect_type_ident();
        let mut bounds = Vec::new();
        if self.eat(&TokenKind::Colon).is_some() {
            bounds.push(self.expect_type_ident());
            while self.eat(&TokenKind::Ampersand).is_some() {
                bounds.push(self.expect_type_ident());
            }
        }
        TypeParam { name, bounds, span }
    }

    pub(crate) fn parse_block(&mut self) -> Vec<Statement> {
        let mut stmts = Vec::new();
        self.skip_newlines();
        while !self.at(&TokenKind::End) && !self.at(&TokenKind::Else) && !self.at_eof() {
            let before = self.pos;
            stmts.push(self.parse_statement());
            if self.pos == before {
                self.error(
                    format!("unexpected token {:?}", self.peek()),
                    self.current_span(),
                );
                self.advance();
            }
            self.skip_newlines();
        }
        stmts
    }
}
