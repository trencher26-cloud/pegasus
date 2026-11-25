//! S-expression rendering of expressions, in the corpus `shape` notation
//! (tests/grammar/README.md). Parentheses are transparent; types render as
//! their source text.

use crate::ast::*;

pub fn render(e: &Expr) -> String {
    let mut out = String::new();
    expr(e, &mut out);
    out
}

fn list(out: &mut String, head: &str, f: impl FnOnce(&mut String)) {
    out.push('(');
    out.push_str(head);
    f(out);
    out.push(')');
}

fn sp_expr(e: &Expr, out: &mut String) {
    out.push(' ');
    expr(e, out);
}

fn generic_args(args: &[GenericArg], out: &mut String) {
    out.push_str(" (");
    for (i, a) in args.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        match a {
            GenericArg::Type(t) => out.push_str(&t.text),
            GenericArg::Unit(s) | GenericArg::Const(s) => out.push_str(s),
        }
    }
    out.push(')');
}

fn call_args(args: &[Arg], out: &mut String) {
    for a in args {
        sp_expr(&a.value, out);
    }
}

fn pattern(p: &Pattern) -> String {
    match &p.kind {
        PatternKind::Ident { name, .. } => name.clone(),
        PatternKind::Wildcard => "_".into(),
        PatternKind::Tuple(ps) => {
            let inner: Vec<String> = ps.iter().map(pattern).collect();
            format!("(tuple {})", inner.join(" "))
        }
        PatternKind::Literal(s) => s.clone(),
        _ => "pat".into(),
    }
}

fn expr(e: &Expr, out: &mut String) {
    match &e.kind {
        ExprKind::Name(n) | ExprKind::SelfValue(n) | ExprKind::Literal(n) => out.push_str(n),
        ExprKind::Interp { text, .. } => out.push_str(text),
        ExprKind::GenericName { name, args } => list(out, "gpath ", |o| {
            o.push_str(name);
            generic_args(args, o);
        }),
        ExprKind::Field { recv, name, args } => match args {
            Some(a) => list(out, "gfield", |o| {
                sp_expr(recv, o);
                o.push(' ');
                o.push_str(name);
                generic_args(a, o);
            }),
            None => list(out, "field", |o| {
                sp_expr(recv, o);
                o.push(' ');
                o.push_str(name);
            }),
        },
        ExprKind::TupleField { recv, index } => list(out, "field", |o| {
            sp_expr(recv, o);
            o.push(' ');
            o.push_str(index);
        }),
        ExprKind::Call { callee, args } => match &callee.kind {
            ExprKind::Field {
                recv,
                name,
                args: None,
            } => list(out, "mcall", |o| {
                sp_expr(recv, o);
                o.push(' ');
                o.push_str(name);
                call_args(args, o);
            }),
            ExprKind::Field {
                recv,
                name,
                args: Some(g),
            } => list(out, "gmcall", |o| {
                sp_expr(recv, o);
                o.push(' ');
                o.push_str(name);
                generic_args(g, o);
                call_args(args, o);
            }),
            ExprKind::GenericName { name, args: g } => list(out, "gcall ", |o| {
                o.push_str(name);
                generic_args(g, o);
                call_args(args, o);
            }),
            _ => list(out, "call", |o| {
                sp_expr(callee, o);
                call_args(args, o);
            }),
        },
        ExprKind::Index { recv, index } => list(out, "index", |o| {
            sp_expr(recv, o);
            sp_expr(index, o);
        }),
        ExprKind::Try(x) => list(out, "try", |o| sp_expr(x, o)),
        ExprKind::Await(x) => list(out, "await", |o| sp_expr(x, o)),
        ExprKind::Unary { op, expr: x } => list(out, op, |o| sp_expr(x, o)),
        ExprKind::Binary { op, lhs, rhs } => list(out, op, |o| {
            sp_expr(lhs, o);
            sp_expr(rhs, o);
        }),
        ExprKind::Cast { expr: x, ty } => list(out, "cast", |o| {
            sp_expr(x, o);
            o.push(' ');
            o.push_str(&ty.text);
        }),
        ExprKind::Range { lo, hi, inclusive } => {
            list(out, if *inclusive { "range=" } else { "range" }, |o| {
                for b in [lo, hi] {
                    match b {
                        Some(x) => sp_expr(x, o),
                        None => o.push_str(" _"),
                    }
                }
            })
        }
        ExprKind::Closure { params, body, .. } => list(out, "closure (", |o| {
            let ps: Vec<String> = params.iter().map(|(p, _)| pattern(p)).collect();
            o.push_str(&ps.join(" "));
            o.push(')');
            sp_expr(body, o);
        }),
        ExprKind::Struct { path, fields, .. } => list(out, "struct", |o| {
            sp_expr(path, o);
            for (name, value) in fields {
                o.push_str(" (");
                o.push_str(name);
                if let Some(v) = value {
                    sp_expr(v, o);
                }
                o.push(')');
            }
        }),
        ExprKind::Tuple(xs) => list(out, "tuple", |o| xs.iter().for_each(|x| sp_expr(x, o))),
        ExprKind::Array(xs) => list(out, "array", |o| xs.iter().for_each(|x| sp_expr(x, o))),
        ExprKind::Repeat { value, count } => list(out, "repeat", |o| {
            sp_expr(value, o);
            sp_expr(count, o);
        }),
        ExprKind::Unit => out.push_str("()"),
        ExprKind::Block(_) => out.push_str("(block)"),
        ExprKind::Comptime(_) => out.push_str("(comptime)"),
        ExprKind::Unsafe { .. } => out.push_str("(unsafe)"),
        ExprKind::If { .. } => out.push_str("(if)"),
        ExprKind::Match { .. } => out.push_str("(match)"),
        ExprKind::While { .. } => out.push_str("(while)"),
        ExprKind::Loop(_) => out.push_str("(loop)"),
        ExprKind::For { .. } => out.push_str("(for)"),
        ExprKind::Return(x) => list(out, "return", |o| {
            if let Some(x) = x {
                sp_expr(x, o)
            }
        }),
        ExprKind::Break(x) => list(out, "break", |o| {
            if let Some(x) = x {
                sp_expr(x, o)
            }
        }),
        ExprKind::Continue => out.push_str("continue"),
        ExprKind::Error => out.push_str("<error>"),
    }
}
