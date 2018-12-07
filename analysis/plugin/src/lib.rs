#![feature(plugin_registrar, rustc_private, quote)]

extern crate rustc_plugin;
extern crate syntax;

use std::collections::HashSet;

use rustc_plugin::Registry;
use syntax::ast;
use syntax::ext::base::{SyntaxExtension, ExtCtxt, Annotatable, MultiItemModifier};
use syntax::ext::build::AstBuilder;
use syntax::fold::{self, Folder};
use syntax::ptr::P;
use syntax::symbol::{Symbol, Ident};
use syntax::source_map::{Span, DUMMY_SP};
use syntax::util::move_map::MoveMap;

#[plugin_registrar]
pub fn plugin_registrar(reg: &mut Registry) {
    let plugin = LifetimeAnalysis::new(reg.args());
    reg.register_syntax_extension(
        Symbol::intern("lifetime_analysis"),
        SyntaxExtension::MultiModifier(Box::new(plugin)));
}

struct LifetimeAnalysis {
}

impl LifetimeAnalysis {
    fn new(_args: &[ast::NestedMetaItem]) -> Self {
        Self { }
    }
}

impl MultiItemModifier for LifetimeAnalysis {
    fn expand(
        &self,
        cx: &mut ExtCtxt,
        _sp: Span,
        _mi: &ast::MetaItem,
        item: Annotatable
    ) -> Vec<Annotatable> {
        match item {
            Annotatable::Item(i) => {
                match &i.node {
                    ast::ItemKind::Mod(_) => {
                        let mut folder = LifetimeInstrumentation::new(cx);
                        folder.fold_item(i)
                    }
                    _ => panic!("unexpected item: {:#?}", i),
                }.into_iter().map(|i| Annotatable::Item(i)).collect()
            }
            // TODO: handle TraitItem
            // TODO: handle ImplItem
            _ => panic!("unexpected item: {:?}", item),
        }
    }
}

const HOOKED_FUNCTIONS: &[&'static str] = &[
    "malloc",
    "realloc",
];


struct LifetimeInstrumentation<'a, 'cx: 'a> {
    cx: &'a mut ExtCtxt<'cx>,
    hooked_functions: HashSet<Ident>,
}

impl<'a, 'cx> LifetimeInstrumentation<'a, 'cx> {
    fn new(cx: &'a mut ExtCtxt<'cx>) -> Self {
        Self {
            cx,
            hooked_functions: HashSet::new(),
        }
    }

    fn hooked_fn_name(&self, callee: &ast::Expr) -> Option<ast::Ident> {
        match &callee.node {
            ast::ExprKind::Path(None, path) => {
                if path.segments.len() == 1 &&
                    self.hooked_functions.contains(&path.segments[0].ident)
                {
                    return Some(path.segments[0].ident);
                }
            }
            _ => (),
        }

        None
    }
}

impl<'a, 'cx> Folder for LifetimeInstrumentation<'a, 'cx> {
    // Will be needed for other crates?
    // fn fold_mod(&mut self, m: ast::Mod) -> ast::Mod {
    //     let mut items = vec![
    //         quote_item!(self.cx, extern crate c2rust_analysis_rt;).unwrap(),
    //         quote_item!(self.cx, use c2rust_analysis_rt::*;).unwrap(),
    //     ];
    //     items.extend(m.items.move_flat_map(|x| self.fold_item(x)));
    //     ast::Mod {
    //         items,
    //         ..m
    //     }
    // }

    fn fold_foreign_item_simple(&mut self, item: ast::ForeignItem) -> ast::ForeignItem {
        if let ast::ForeignItemKind::Fn(_, _) = &item.node {
            if HOOKED_FUNCTIONS.contains(&&*item.ident.name.as_str()) {
                self.hooked_functions.insert(item.ident);
            }
        }
        fold::noop_fold_foreign_item_simple(item, self)
    }

    fn fold_expr(&mut self, expr: P<ast::Expr>) -> P<ast::Expr> {
        match &expr.node {
            ast::ExprKind::Call(callee, args) => {
                if let Some(fn_name) = self.hooked_fn_name(callee) {
                    // fold arguments in case we need to hook a subexpression
                    let args = self.fold_exprs(args.clone());

                    // Cast all original arguments to usize
                    let mut runtime_args: Vec<P<ast::Expr>> = args
                        .iter()
                        .map(|arg| quote_expr!(self.cx, $arg as usize))
                        .collect();
                    // Add the return value of the hooked call.
                    runtime_args.push(quote_expr!(self.cx, ret as usize));

                    // Build a replacement call with the folded args
                    let call = self.cx.expr_call(
                        expr.span,
                        callee.clone(),
                        args,
                    );

                    // Build the hook call (we can't do this with quoting
                    // because I couldn't figure out how to get quote_expr! to
                    // play nice with multiple arguments in a variable.
                    let hook_call = self.cx.expr_call(
                        DUMMY_SP,
                        quote_expr!(self.cx, c2rust_analysis_rt::$fn_name),
                        runtime_args,
                    );

                    // Build the instrumentation block
                    return quote_expr!(self.cx, {
                        let ret = $call;
                        $hook_call;
                        ret
                    });
                }
            }
            _ => (),
        }

        expr.map(|expr| fold::noop_fold_expr(expr, self))
    }
}
