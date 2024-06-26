//! A MIR pass which duplicates a coroutine's body and removes any derefs which
//! would be present for upvars that are taken by-ref. The result of which will
//! be a coroutine body that takes all of its upvars by-move, and which we stash
//! into the `CoroutineInfo` for all coroutines returned by coroutine-closures.

use rustc_data_structures::unord::UnordSet;
use rustc_hir as hir;
use rustc_middle::mir::visit::MutVisitor;
use rustc_middle::mir::{self, dump_mir, MirPass};
use rustc_middle::ty::{self, InstanceDef, Ty, TyCtxt, TypeVisitableExt};
use rustc_target::abi::FieldIdx;

pub struct ByMoveBody;

impl<'tcx> MirPass<'tcx> for ByMoveBody {
    fn run_pass(&self, tcx: TyCtxt<'tcx>, body: &mut mir::Body<'tcx>) {
        let Some(coroutine_def_id) = body.source.def_id().as_local() else {
            return;
        };
        let Some(hir::CoroutineKind::Desugared(_, hir::CoroutineSource::Closure)) =
            tcx.coroutine_kind(coroutine_def_id)
        else {
            return;
        };
        let coroutine_ty = body.local_decls[ty::CAPTURE_STRUCT_LOCAL].ty;
        if coroutine_ty.references_error() {
            return;
        }
        let ty::Coroutine(_, args) = *coroutine_ty.kind() else { bug!("{body:#?}") };

        let coroutine_kind = args.as_coroutine().kind_ty().to_opt_closure_kind().unwrap();
        if coroutine_kind == ty::ClosureKind::FnOnce {
            return;
        }

        let mut by_ref_fields = UnordSet::default();
        let by_move_upvars = Ty::new_tup_from_iter(
            tcx,
            tcx.closure_captures(coroutine_def_id).iter().enumerate().map(|(idx, capture)| {
                if capture.is_by_ref() {
                    by_ref_fields.insert(FieldIdx::from_usize(idx));
                }
                capture.place.ty()
            }),
        );
        let by_move_coroutine_ty = Ty::new_coroutine(
            tcx,
            coroutine_def_id.to_def_id(),
            ty::CoroutineArgs::new(
                tcx,
                ty::CoroutineArgsParts {
                    parent_args: args.as_coroutine().parent_args(),
                    kind_ty: Ty::from_closure_kind(tcx, ty::ClosureKind::FnOnce),
                    resume_ty: args.as_coroutine().resume_ty(),
                    yield_ty: args.as_coroutine().yield_ty(),
                    return_ty: args.as_coroutine().return_ty(),
                    witness: args.as_coroutine().witness(),
                    tupled_upvars_ty: by_move_upvars,
                },
            )
            .args,
        );

        let mut by_move_body = body.clone();
        MakeByMoveBody { tcx, by_ref_fields, by_move_coroutine_ty }.visit_body(&mut by_move_body);
        dump_mir(tcx, false, "coroutine_by_move", &0, &by_move_body, |_, _| Ok(()));
        by_move_body.source = mir::MirSource::from_instance(InstanceDef::CoroutineKindShim {
            coroutine_def_id: coroutine_def_id.to_def_id(),
        });
        body.coroutine.as_mut().unwrap().by_move_body = Some(by_move_body);
    }
}

struct MakeByMoveBody<'tcx> {
    tcx: TyCtxt<'tcx>,
    by_ref_fields: UnordSet<FieldIdx>,
    by_move_coroutine_ty: Ty<'tcx>,
}

impl<'tcx> MutVisitor<'tcx> for MakeByMoveBody<'tcx> {
    fn tcx(&self) -> TyCtxt<'tcx> {
        self.tcx
    }

    fn visit_place(
        &mut self,
        place: &mut mir::Place<'tcx>,
        context: mir::visit::PlaceContext,
        location: mir::Location,
    ) {
        if place.local == ty::CAPTURE_STRUCT_LOCAL
            && let Some((&mir::ProjectionElem::Field(idx, ty), projection)) =
                place.projection.split_first()
            && self.by_ref_fields.contains(&idx)
        {
            let (begin, end) = projection.split_first().unwrap();
            // FIXME(async_closures): I'm actually a bit surprised to see that we always
            // initially deref the by-ref upvars. If this is not actually true, then we
            // will at least get an ICE that explains why this isn't true :^)
            assert_eq!(*begin, mir::ProjectionElem::Deref);
            // Peel one ref off of the ty.
            let peeled_ty = ty.builtin_deref(true).unwrap().ty;
            *place = mir::Place {
                local: place.local,
                projection: self.tcx.mk_place_elems_from_iter(
                    [mir::ProjectionElem::Field(idx, peeled_ty)]
                        .into_iter()
                        .chain(end.iter().copied()),
                ),
            };
        }
        self.super_place(place, context, location);
    }

    fn visit_local_decl(&mut self, local: mir::Local, local_decl: &mut mir::LocalDecl<'tcx>) {
        // Replace the type of the self arg.
        if local == ty::CAPTURE_STRUCT_LOCAL {
            local_decl.ty = self.by_move_coroutine_ty;
        }
    }
}
