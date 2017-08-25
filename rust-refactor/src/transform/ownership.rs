use std::collections::HashMap;
use std::collections::HashSet;

use rustc::hir;
use rustc_data_structures::indexed_vec::IndexVec;
use syntax::ast::*;
use syntax::attr;
use syntax::codemap::DUMMY_SP;
use syntax::fold::{self, Folder};
use syntax::parse::token::{self, Token, DelimToken};
use syntax::ptr::P;
use syntax::symbol::Symbol;
use syntax::tokenstream::{TokenTree, TokenStream, Delimited};
use syntax::util::small_vector::SmallVector;
use syntax::util::move_map::MoveMap;

use analysis::ownership::{self, ConcretePerm, Var, PTy};
use analysis::ownership::constraint::{ConstraintSet, Perm};
use api::*;
use command::{CommandState, Registry, DriverCommand};
use driver::{self, Phase};
use fold::Fold;
use make_ast::mk;
use util::IntoSymbol;

pub fn register_commands(reg: &mut Registry) {
    reg.register("ownership_annotate", |args| {
        let label = args.get(0).map_or("target", |x| x).into_symbol();

        Box::new(DriverCommand::new(Phase::Phase3, move |st, cx| {
            do_annotate(st, cx, label);
        }))
    });

    reg.register("ownership_split_variants", |args| {
        let label = args.get(0).map_or("target", |x| x).into_symbol();

        Box::new(DriverCommand::new(Phase::Phase3, move |st, cx| {
            do_split_variants(st, cx, label);
        }))
    });
}

fn do_annotate(st: &CommandState,
               cx: &driver::Ctxt,
               label: Symbol) {
    let analysis = ownership::analyze(&st, &cx);

    struct AnnotateFolder<'a, 'hir: 'a, 'tcx> {
        label: Symbol,
        ana: ownership::AnalysisResult<'tcx>,
        hir_map: &'a hir::map::Map<'hir>,
        st: &'a CommandState,
    }

    impl<'a, 'hir, 'tcx> AnnotateFolder<'a, 'hir, 'tcx> {
        fn static_attr_for(&self, id: NodeId) -> Option<Attribute> {
            self.hir_map.opt_local_def_id(id)
                .and_then(|def_id| self.ana.statics.get(&def_id))
                .and_then(|&ty| build_static_attr(ty))
        }

        fn constraints_attr_for(&self, id: NodeId) -> Option<Attribute> {
            self.hir_map.opt_local_def_id(id)
                .and_then(|def_id| self.ana.fns.get(&def_id))
                .map(|fr| build_constraints_attr(&fr.cset))
        }

        fn push_mono_attrs_for(&self, id: NodeId, dest: &mut Vec<Attribute>) {
            if let Some(fr) = self.hir_map.opt_local_def_id(id)
                    .and_then(|def_id| self.ana.fns.get(&def_id)) {
                if fr.num_sig_vars == 0 {
                    return;
                }

                for mr in &fr.monos {
                    dest.push(build_mono_attr(&mr.suffix, &mr.assign));
                }
            }
        }

        fn clean_attrs(&self, attrs: &mut Vec<Attribute>) {
            attrs.retain(|a| {
                if let Some(name) = a.name() {
                    match &name.as_str() as &str {
                        "ownership_mono" |
                        "ownership_constraints" |
                        "ownership_static" => return false,
                        _ => {},
                    }
                }
                true
            });
        }
    }

    impl<'a, 'hir, 'tcx> Folder for AnnotateFolder<'a, 'hir, 'tcx> {
        fn fold_item(&mut self, i: P<Item>) -> SmallVector<P<Item>> {
            if !self.st.marked(i.id, self.label) {
                return fold::noop_fold_item(i, self);
            }

            fold::noop_fold_item(i.map(|mut i| {
                match i.node {
                    ItemKind::Static(..) | ItemKind::Const(..) => {
                        self.clean_attrs(&mut i.attrs);
                        if let Some(attr) = self.static_attr_for(i.id) {
                            i.attrs.push(attr);
                        }
                    },

                    ItemKind::Fn(..) => {
                        self.clean_attrs(&mut i.attrs);
                        if let Some(attr) = self.constraints_attr_for(i.id) {
                            i.attrs.push(attr);
                        }
                        self.push_mono_attrs_for(i.id, &mut i.attrs);
                    },

                    _ => {},
                }

                i
            }), self)
        }

        fn fold_impl_item(&mut self, i: ImplItem) -> SmallVector<ImplItem> {
            if !self.st.marked(i.id, self.label) {
                return fold::noop_fold_impl_item(i, self);
            }

            fold::noop_fold_impl_item(i, self)
        }

        fn fold_struct_field(&mut self, mut sf: StructField) -> StructField {
            if !self.st.marked(sf.id, self.label) {
                return fold::noop_fold_struct_field(sf, self);
            }

            self.clean_attrs(&mut sf.attrs);
            if let Some(attr) = self.static_attr_for(sf.id) {
                sf.attrs.push(attr);
            }

            fold::noop_fold_struct_field(sf, self)
        }
    }

    st.map_krate(|krate| {
        krate.fold(&mut AnnotateFolder {
            label: label,
            ana: analysis,
            hir_map: cx.hir_map(),
            st: st,
        })
    });
}

fn build_static_attr(ty: PTy) -> Option<Attribute> {
    let mut args = Vec::new();
    ty.for_each_label(&mut |p| {
        if let Some(p) = *p {
            args.push(perm_token(p));
        }
    });
    let tokens = parens(args).into();
    Some(make_attr("ownership_static", tokens))
}

fn build_constraints_attr(cset: &ConstraintSet) -> Attribute {
    let mut args = Vec::new();

    fn push_perm_tokens(p: Perm, dest: &mut Vec<TokenTree>) {
        match p {
            Perm::Concrete(p) => dest.push(perm_token(p)),
            Perm::SigVar(v) => dest.push(ident_token(&format!("_{}", v.0))),
            Perm::Min(ps) => {
                let mut ts = Vec::new();
                for (i, &p) in ps.iter().enumerate() {
                    if i > 0 {
                        ts.push(token(Token::Comma));
                    }
                    push_perm_tokens(p, &mut ts);
                }
                dest.push(ident_token("min"));
                dest.push(parens(ts));
            },
            _ => panic!("unexpected var kind in fn constraints"),
        }
    }

    for (i, &(a, b)) in cset.iter().enumerate() {
        if i > 0 {
            args.push(token(Token::Comma));
        }
        args.push(ident_token("le"));

        let mut le_args = Vec::new();
        push_perm_tokens(a, &mut le_args);
        le_args.push(token(Token::Comma));
        push_perm_tokens(b, &mut le_args);

        args.push(parens(le_args));
    }

    let tokens = parens(args).into();
    make_attr("ownership_constraints", tokens)
}

fn build_mono_attr(suffix: &str, assign: &IndexVec<Var, ConcretePerm>) -> Attribute {
    let mut args = Vec::new();
    args.push(str_token(suffix));

    for &p in assign.iter() {
        args.push(token(Token::Comma));
        args.push(perm_token(p));
    }

    let tokens = parens(args).into();
    make_attr("ownership_mono", tokens)
}

fn perm_token(p: ConcretePerm) -> TokenTree {
    let name = match p {
        ConcretePerm::Read => "READ",
        ConcretePerm::Write => "WRITE",
        ConcretePerm::Move => "MOVE",
    };
    TokenTree::Token(DUMMY_SP, Token::Ident(mk().ident(name)))
}

fn ident_token(name: &str) -> TokenTree {
    token(Token::Ident(mk().ident(name)))
}

fn str_token(s: &str) -> TokenTree {
    token(Token::Literal(token::Lit::Str_(s.into_symbol()), None))
}

fn token(t: Token) -> TokenTree {
    TokenTree::Token(DUMMY_SP, t)
}

fn parens(ts: Vec<TokenTree>) -> TokenTree {
    TokenTree::Delimited(DUMMY_SP, Delimited {
        delim: DelimToken::Paren,
        tts: ts.into_iter().collect::<TokenStream>().into(),
    })
}

fn make_attr(name: &str, tokens: TokenStream) -> Attribute {
    Attribute {
        id: AttrId(0),
        style: AttrStyle::Outer,
        path: mk().path(vec![name]),
        tokens: tokens,
        is_sugared_doc: false,
        span: DUMMY_SP,
    }
}

fn build_variant_attr(group: &str) -> Attribute {
    let tokens = parens(vec![str_token(group)]).into();
    make_attr("ownership_variant_of", tokens)
}



fn do_split_variants(st: &CommandState,
                     cx: &driver::Ctxt,
                     label: Symbol) {
    let ana = ownership::analyze(&st, &cx);

    // Map from ExprPath/ExprMethodCall span to function ref idx within the caller.
    let mut span_fref_idx = HashMap::new();
    for fr in ana.fns.values() {
        for (idx, fref) in fr.func_refs.iter().enumerate() {
            if let Some(span) = fref.span {
                span_fref_idx.insert(span, idx);
            }
        }
    }

    let mut handled_spans = HashSet::new();

    st.map_krate(|krate| {
        // (1) Duplicate marked fns with `mono` attrs to produce multiple variants.  We rewrite
        // references to other fns during this process, since afterward it would be difficult to
        // distinguish the different copies - their bodies have identical spans and `NodeId`s.
        let krate = ::fn_edit::fold_fns_multi(krate, |fl| {
            if !st.marked(fl.id, label) {
                return SmallVector::one(fl);
            }
            eprintln!("looking at {:?}", fl.ident);

            let def_id = match_or!([cx.hir_map().opt_local_def_id(fl.id)]
                                   Some(x) => x; return SmallVector::one(fl));
            let fr = match_or!([ana.fns.get(&def_id)]
                               Some(x) => x; return SmallVector::one(fl));

            let path_str = cx.ty_ctxt().def_path(def_id).to_string(cx.ty_ctxt());

            fr.monos.iter().enumerate().map(|(mono_idx, mr)| {
                let mut fl = fl.clone();

                if mr.suffix.len() > 0 {
                    fl.ident = mk().ident(format!("{}_{}", fl.ident.name, mr.suffix));
                }

                // Delete all but one of the `#[ownership_mono]` annotations.
                fl.attrs.retain(|a| {
                    !a.check_name("ownership_mono") &&
                    (!a.check_name("ownership_constraints") || mono_idx == 0)
                });
                fl.attrs.push(build_mono_attr(&mr.suffix, &mr.assign));
                fl.attrs.push(build_variant_attr(&path_str));

                fl.block = fl.block.map(|b| fold_nodes(b, |e: P<Expr>| {
                    let fref_idx = match_or!([span_fref_idx.get(&e.span)]
                                             Some(&x) => x; return e);
                    handled_spans.insert(e.span);

                    let callee = fr.func_refs[fref_idx].def_id;
                    let callee_marked = cx.hir_map().as_local_node_id(callee)
                        .map_or(false, |id| st.marked(id, label));
                    if !callee_marked {
                        // A call from a split function to a non-split function.  Leave the call
                        // unchanged.
                        return e;
                    }
                    let callee_mono_idx = mr.callee_mono_idxs[fref_idx];
                    let callee_mono = &ana.fns[&callee].monos[callee_mono_idx];

                    apply_suffix(&callee_mono.suffix, e)
                }));

                fl
            }).collect()
        });

        let krate = fold_nodes(krate, |e: P<Expr>| {
            let fref_idx = match_or!([span_fref_idx.get(&e.span)]
                                     Some(&x) => x; return e);
            if handled_spans.contains(&e.span) {
                // This span was handled while splitting a function into variants.
                return e;
            }

            // Figure out what we're calling.
            let src = cx.hir_map().get_parent(e.id);
            let src_def_id = cx.node_def_id(src);
            let src_fr = &ana.fns[&src_def_id];
            let dest = src_fr.func_refs[fref_idx].def_id;

            let dest_marked = cx.hir_map().as_local_node_id(dest)
                .map_or(false, |id| st.marked(id, label));
            if !dest_marked {
                // A call to a non-split function.  Leave the call unchanged.
                return e;
            }
            // This is a call from a non-split fn to a split fn.  We don't have a specific (src)
            // `mono_idx` to use to look up the dest mono idx.  So we just look at the src's
            // first variant instead.  This will usually give us the highest-permission dest mono
            // that is used in any of the src variants.
            let src_mr = &src_fr.monos[0];
            let dest_mono_idx = src_mr.callee_mono_idxs[fref_idx];
            let dest_mono = &ana.fns[&dest].monos[dest_mono_idx];

            apply_suffix(&dest_mono.suffix, e)
        });

        krate
    });
}

fn apply_suffix(suffix: &str, e: P<Expr>) -> P<Expr> {
    e.map(|mut e| {
        match e.node {
            ExprKind::Path(_, ref mut path) => {
                // Append the suffix to the last path segment.
                let seg = path.segments.last_mut().unwrap();
                let new_name = mono_name(&seg.identifier.name.as_str(), &suffix);
                seg.identifier = mk().ident(&new_name);
            },

            ExprKind::MethodCall(ref mut seg, _) => {
                let new_name = mono_name(&seg.identifier.name.as_str(), &suffix);
                seg.identifier = mk().ident(&new_name);
            },

            _ => panic!("apply_suffix: unexpected expr kind: {:?}", e),
        }

        e
    })
}

fn mono_name(base: &str, suffix: &str) -> String {
    if suffix.len() == 0 {
        format!("{}", base)
    } else {
        format!("{}_{}", base, suffix)
    }
}


